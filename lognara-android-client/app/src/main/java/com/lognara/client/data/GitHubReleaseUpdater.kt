package com.lognara.client.data

import android.app.PendingIntent
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import com.lognara.client.BuildConfig
import com.lognara.client.R
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import okhttp3.Call
import okhttp3.Callback
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

fun interface ApkPackageVerifier {
    fun verify(apk: File, expectedVersionCode: Int)
}

class AndroidApkPackageVerifier(context: Context) : ApkPackageVerifier {
    private val packageManager = context.applicationContext.packageManager
    private val packageName = context.applicationContext.packageName

    @Suppress("DEPRECATION")
    override fun verify(apk: File, expectedVersionCode: Int) {
        val archive = packageManager.getPackageArchiveInfo(apk.absolutePath, PackageManager.GET_SIGNING_CERTIFICATES)
            ?: throw UpdateFailure(R.string.app_update_error_invalid_apk)
        if (archive.packageName != packageName) throw UpdateFailure(R.string.app_update_error_wrong_package)
        if (archive.longVersionCode != expectedVersionCode.toLong()) {
            throw UpdateFailure(R.string.app_update_error_version_mismatch)
        }

        val archiveSigning = archive.signingInfo ?: throw UpdateFailure(R.string.app_update_error_missing_signature)
        val archiveSigners = archiveSigning.apkContentsSigners
        if (archiveSigners.isNullOrEmpty()) throw UpdateFailure(R.string.app_update_error_missing_signature)

        val installedInfo = packageManager.getPackageInfo(packageName, PackageManager.GET_SIGNING_CERTIFICATES)
        val installedSigning = installedInfo.signingInfo ?: throw UpdateFailure(R.string.app_update_error_signature_check)
        val signatureMatches = if (archiveSigning.hasMultipleSigners() != installedSigning.hasMultipleSigners()) {
            false
        } else if (archiveSigning.hasMultipleSigners()) {
            val installedSigners = installedSigning.apkContentsSigners.orEmpty()
            archiveSigners.size == installedSigners.size && archiveSigners.all { archiveSignature ->
                installedSigners.any { installedSignature -> installedSignature == archiveSignature }
            }
        } else {
            archiveSigners.size == 1 && packageManager.hasSigningCertificate(
                packageName,
                archiveSigners.single().toByteArray(),
                PackageManager.CERT_INPUT_RAW_X509,
            )
        }
        if (!signatureMatches) throw UpdateFailure(R.string.app_update_error_signature_mismatch)
    }
}

sealed interface InstallerEvent {
    data object UserActionRequired : InstallerEvent
    data object Succeeded : InstallerEvent
    data class Failed(val messageResource: Int) : InstallerEvent
}

class GitHubReleaseUpdater(
    context: Context,
    private val client: OkHttpClient = githubClient(),
    private val apiBaseUrl: String = "https://api.github.com",
    private val repository: String = BuildConfig.UPDATE_REPOSITORY,
    private val installedVersionCode: Int = BuildConfig.VERSION_CODE,
    private val packageVerifier: ApkPackageVerifier = AndroidApkPackageVerifier(context),
) {
    private val appContext = context.applicationContext
    private val installerEventMutable = MutableStateFlow<InstallerEvent?>(null)
    val installerEvent = installerEventMutable.asStateFlow()
    private val deferredInstallerAction = DeferredInstallerAction<Intent>()

    fun clearInstallerEvent(event: InstallerEvent) {
        installerEventMutable.compareAndSet(event, null)
    }

    fun onAppResumed() {
        deferredInstallerAction.onResumed()?.let { startInstallConfirmation(appContext, it, reportPendingEvent = false) }
    }

    fun onAppPaused() {
        deferredInstallerAction.onPaused()
    }

    suspend fun checkForUpdate(channel: UpdateChannel): AvailableUpdate? {
        val eligible = mutableListOf<GithubRelease>()
        for (page in 1..MAX_RELEASE_PAGES) {
            val releases = parseGithubReleases(requestText(releasesRequest(page), RELEASE_LIST_MAX_BYTES))
            eligible += releasesForChannel(releases, channel)
            if (eligible.size >= RELEASE_MANIFESTS_TO_CHECK || releases.size < RELEASE_PAGE_SIZE) break
        }
        val candidates = mutableListOf<AvailableUpdate>()
        var metadataProblem: UpdateFailure? = null
        for (release in eligible.sortedByDescending { it.createdAt }.take(RELEASE_MANIFESTS_TO_CHECK)) {
            val versionAsset = release.assets.firstOrNull { it.name == VERSION_JSON_ASSET }
            val apkAsset = release.assets.firstOrNull { it.name == ANDROID_APK_ASSET }
            if (versionAsset == null || apkAsset == null) {
                metadataProblem = UpdateFailure(R.string.app_update_error_release_assets)
                continue
            }
            try {
                val manifest = parseReleaseManifest(requestText(assetRequest(versionAsset), VERSION_JSON_MAX_BYTES))
                if (manifest.channel != channelForTag(release.tagName)) {
                    metadataProblem = UpdateFailure(R.string.app_update_error_tag_channel_mismatch)
                    continue
                }
                candidates += AvailableUpdate(release, manifest, apkAsset)
            } catch (failure: UpdateFailure) {
                if (failure.temporary) throw failure
                // Ignore malformed or incomplete releases and continue with the next recent candidate.
                metadataProblem = failure
            }
        }
        metadataFailureWithoutValidCandidate(metadataProblem, candidates.size)?.let { throw it }
        return chooseNewestUpdate(candidates, installedVersionCode)
    }

    suspend fun downloadAndVerify(update: AvailableUpdate, onProgress: (Long, Long?) -> Unit): File =
        withContext(Dispatchers.IO) {
            requireHttps(update.apkAsset.downloadUrl)
            val directory = File(appContext.cacheDir, "updates")
            if (!directory.exists() && !directory.mkdirs()) {
                throw UpdateFailure(R.string.app_update_error_cache_directory)
            }
            val partialFile = File(directory, "lognara-android.apk.part")
            val apkFile = File(directory, ANDROID_APK_ASSET)
            partialFile.delete()
            apkFile.delete()

            try {
                val response = executeResponse(assetRequest(update.apkAsset))
                response.use {
                    val body = response.body
                    val contentLength = body.contentLength().takeIf { it >= 0 }
                    val actualSha256 = writeApkBody(
                        body.byteStream(),
                        partialFile,
                        contentLength,
                        APK_MAX_BYTES,
                        R.string.app_update_error_apk_too_large,
                        onProgress,
                    )
                    verifySha256Hex(actualSha256, update.manifest.sha256)
                    if (!partialFile.renameTo(apkFile)) {
                        throw UpdateFailure(R.string.app_update_error_save_apk)
                    }
                }
                packageVerifier.verify(apkFile, update.manifest.versionCode)
                apkFile
            } catch (failure: Exception) {
                partialFile.delete()
                apkFile.delete()
                throw failure
            }
        }

    fun canRequestPackageInstalls(): Boolean = appContext.packageManager.canRequestPackageInstalls()

    suspend fun install(apkFile: File) = withContext(kotlinx.coroutines.Dispatchers.IO) {
        val installer = appContext.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
            .apply { setAppPackageName(appContext.packageName) }
        val sessionId = installer.createSession(params)
        try {
            installer.openSession(sessionId).use { session ->
                session.openWrite(ANDROID_APK_ASSET, 0, apkFile.length()).use { sessionOutput ->
                    FileInputStream(apkFile).use { apkInput ->
                        apkInput.copyTo(sessionOutput)
                    }
                    session.fsync(sessionOutput)
                }
                val callbackIntent = Intent(appContext, UpdateInstallReceiver::class.java)
                    .setAction(INSTALL_RESULT_ACTION)
                val callback = PendingIntent.getBroadcast(
                    appContext,
                    sessionId,
                    callbackIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
                )
                session.commit(callback.intentSender)
            }
        } catch (failure: Exception) {
            installer.abandonSession(sessionId)
            throw failure
        }
    }

    fun handleInstallerBroadcast(context: Context, intent: Intent) {
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val userAction = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                if (userAction == null) {
                    installerEventMutable.value = InstallerEvent.Failed(R.string.app_update_error_install_confirmation)
                    return
                }
                val actionToLaunch = deferredInstallerAction.receive(Intent(userAction))
                if (actionToLaunch != null) startInstallConfirmation(context, actionToLaunch)
                else installerEventMutable.value = InstallerEvent.UserActionRequired
            }
            PackageInstaller.STATUS_SUCCESS -> installerEventMutable.value = InstallerEvent.Succeeded
            else -> installerEventMutable.value = InstallerEvent.Failed(installerErrorMessage(intent))
        }
    }

    private suspend fun requestText(request: Request, maximumBytes: Int): String =
        execute(request) { response ->
            val body = response.body
            val contentLength = body.contentLength()
            if (contentLength > maximumBytes) throw UpdateFailure(R.string.app_update_error_response_too_large)
            String(readLimited(body.byteStream(), maximumBytes), Charsets.UTF_8)
        }

    private fun releasesRequest(page: Int): Request = Request.Builder()
        .url("$apiBaseUrl/repos/$repository/releases?per_page=$RELEASE_PAGE_SIZE&page=$page")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "Lognara-Android")
        .get()
        .build()

    private fun startInstallConfirmation(
        context: Context,
        userAction: Intent,
        reportPendingEvent: Boolean = true,
    ) {
        userAction.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivity(userAction)
            if (reportPendingEvent) installerEventMutable.value = InstallerEvent.UserActionRequired
        } catch (_: ActivityNotFoundException) {
            installerEventMutable.value = InstallerEvent.Failed(R.string.app_update_error_install_confirmation)
        } catch (_: SecurityException) {
            installerEventMutable.value = InstallerEvent.Failed(R.string.app_update_error_install_security)
        }
    }

    private fun assetRequest(asset: ReleaseAsset): Request {
        requireHttps(asset.downloadUrl)
        return Request.Builder()
            .url(asset.downloadUrl)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "Lognara-Android")
            .get()
            .build()
    }

    private suspend fun <T> execute(request: Request, consume: (Response) -> T): T =
        suspendCancellableCoroutine { continuation ->
            val call = client.newCall(request)
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (continuation.isActive) continuation.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    try {
                        val result = response.use {
                            if (it.code == 403 || it.code == 429) {
                                throw UpdateFailure(R.string.app_update_error_rate_limit, temporary = true)
                            }
                            if (!it.isSuccessful) {
                                throw UpdateFailure(R.string.app_update_error_http_status, listOf(it.code))
                            }
                            consume(it)
                        }
                        if (continuation.isActive) continuation.resume(result)
                    } catch (failure: Exception) {
                        if (continuation.isActive) continuation.resumeWithException(failure)
                    }
                }
            })
        }

    private suspend fun executeResponse(request: Request): Response = suspendCancellableCoroutine { continuation ->
        val call = client.newCall(request)
        continuation.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (continuation.isActive) continuation.resumeWithException(e)
            }

            override fun onResponse(call: Call, response: Response) {
                val failure = when {
                    response.code == 403 || response.code == 429 ->
                        UpdateFailure(R.string.app_update_error_rate_limit, temporary = true)
                    !response.isSuccessful -> UpdateFailure(R.string.app_update_error_http_status, listOf(response.code))
                    else -> null
                }
                if (failure != null) {
                    response.close()
                    if (continuation.isActive) continuation.resumeWithException(failure)
                } else if (continuation.isActive) {
                    continuation.resume(response) { _, value, _ -> value.close() }
                } else {
                    response.close()
                }
            }
        })
    }

    private fun installerErrorMessage(intent: Intent): Int = when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, -1)) {
        PackageInstaller.STATUS_FAILURE_ABORTED -> R.string.app_update_error_install_aborted
        PackageInstaller.STATUS_FAILURE_BLOCKED -> R.string.app_update_error_install_blocked
        PackageInstaller.STATUS_FAILURE_CONFLICT -> R.string.app_update_error_install_conflict
        PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> R.string.app_update_error_install_incompatible
        PackageInstaller.STATUS_FAILURE_INVALID -> R.string.app_update_error_install_invalid
        PackageInstaller.STATUS_FAILURE_STORAGE -> R.string.app_update_error_install_storage
        else -> R.string.app_update_error_install
    }

    private fun channelForTag(tag: String): UpdateChannel =
        if (tag.startsWith("android-dev-")) UpdateChannel.Dev else UpdateChannel.Stable

    private fun requireHttps(url: String) {
        if (!url.startsWith("https://", ignoreCase = true)) throw UpdateFailure(R.string.app_update_error_unsafe_url)
    }

    private companion object {
        const val VERSION_JSON_ASSET = "version.json"
        const val ANDROID_APK_ASSET = "lognara-android.apk"
        const val RELEASE_LIST_MAX_BYTES = 1024 * 1024
        const val RELEASE_PAGE_SIZE = 30
        const val MAX_RELEASE_PAGES = 10
        const val VERSION_JSON_MAX_BYTES = 64 * 1024
        const val APK_MAX_BYTES = 150L * 1024 * 1024
        const val RELEASE_MANIFESTS_TO_CHECK = 5
        const val INSTALL_RESULT_ACTION = "com.lognara.client.UPDATE_INSTALL_RESULT"

        fun githubClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .callTimeout(4, TimeUnit.MINUTES)
            .followRedirects(true)
            .followSslRedirects(false)
            .cache(null)
            .build()
    }
}

class UpdateInstallReceiver : android.content.BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val application = context.applicationContext as? com.lognara.client.LognaraApplication ?: return
        application.container.updater.handleInstallerBroadcast(context, intent)
    }
}

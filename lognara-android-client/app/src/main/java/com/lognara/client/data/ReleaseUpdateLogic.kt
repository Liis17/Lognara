package com.lognara.client.data

import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import com.lognara.client.R
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

data class ReleaseAsset(val name: String, val downloadUrl: String)

data class GithubRelease(
    val tagName: String,
    val draft: Boolean,
    val createdAt: String,
    val assets: List<ReleaseAsset>,
)

data class ReleaseManifest(
    val versionCode: Int,
    val versionName: String,
    val channel: UpdateChannel,
    val commit: String,
    val apk: String,
    val sha256: String,
)

data class AvailableUpdate(
    val release: GithubRelease,
    val manifest: ReleaseManifest,
    val apkAsset: ReleaseAsset,
)

class UpdateFailure(
    val messageResource: Int,
    val formatArgs: List<Any> = emptyList(),
    cause: Throwable? = null,
    val temporary: Boolean = false,
) : IOException(cause)

internal fun parseGithubReleases(jsonText: String): List<GithubRelease> = try {
    Json.parseToJsonElement(jsonText).jsonArray.map { value ->
        val obj = value.jsonObject
        GithubRelease(
            tagName = obj.requiredString("tag_name"),
            draft = obj["draft"]?.jsonPrimitive?.content?.toBooleanStrictOrNull()
                ?: throw UpdateFailure(R.string.app_update_error_release_status),
            createdAt = obj["created_at"]?.jsonPrimitive?.content.orEmpty(),
            assets = obj["assets"]?.jsonArray?.map { assetValue ->
                val asset = assetValue.jsonObject
                ReleaseAsset(
                    name = asset.requiredString("name"),
                    downloadUrl = asset.requiredString("browser_download_url"),
                )
            } ?: emptyList(),
        )
    }
} catch (failure: UpdateFailure) {
    throw failure
} catch (failure: Exception) {
    throw UpdateFailure(R.string.app_update_error_release_list, cause = failure)
}

internal fun parseReleaseManifest(jsonText: String): ReleaseManifest = try {
    val obj = Json.parseToJsonElement(jsonText).jsonObject
    val channel = UpdateChannel.from(obj.requiredString("channel"))
    val channelValue = obj.requiredString("channel")
    if (channelValue !in setOf("stable", "dev")) throw UpdateFailure(R.string.app_update_error_manifest_channel)
    val versionCode = obj["versionCode"]?.jsonPrimitive?.content?.toIntOrNull()
        ?: throw UpdateFailure(R.string.app_update_error_manifest_version)
    val versionName = obj.requiredString("versionName")
    val commit = obj.requiredString("commit")
    val apk = obj.requiredString("apk")
    val sha256 = obj.requiredString("sha256").lowercase()
    if (versionCode <= 0 || versionName.isBlank() || commit.isBlank()) {
        throw UpdateFailure(R.string.app_update_error_manifest_incomplete)
    }
    if (apk != ANDROID_APK_ASSET) throw UpdateFailure(R.string.app_update_error_manifest_apk)
    if (!SHA256_REGEX.matches(sha256)) throw UpdateFailure(R.string.app_update_error_manifest_sha)
    ReleaseManifest(versionCode, versionName, channel, commit, apk, sha256)
} catch (failure: UpdateFailure) {
    throw failure
} catch (failure: Exception) {
    throw UpdateFailure(R.string.app_update_error_manifest, cause = failure)
}

internal fun releasesForChannel(releases: List<GithubRelease>, channel: UpdateChannel): List<GithubRelease> =
    releases.asSequence()
        .filterNot { it.draft }
        .filter { release ->
            val isStableTag = STABLE_TAG_REGEX.matches(release.tagName)
            val isDevTag = DEV_TAG_REGEX.matches(release.tagName)
            when (channel) {
                UpdateChannel.Stable -> isStableTag
                UpdateChannel.Dev -> isStableTag || isDevTag
            }
        }
        .sortedByDescending { it.createdAt }
        .toList()

internal fun chooseNewestUpdate(
    candidates: List<AvailableUpdate>,
    installedVersionCode: Int,
): AvailableUpdate? = candidates
    .asSequence()
    .filter { it.manifest.versionCode > installedVersionCode }
    .maxByOrNull { it.manifest.versionCode }

internal fun isManifestChannelAllowed(manifestChannel: UpdateChannel, selectedChannel: UpdateChannel): Boolean =
    selectedChannel == UpdateChannel.Dev || manifestChannel == UpdateChannel.Stable

internal fun isCurrentUpdateRequest(
    requestChannel: UpdateChannel,
    currentChannel: UpdateChannel,
    requestGeneration: Long,
    currentGeneration: Long,
): Boolean = requestChannel == currentChannel && requestGeneration == currentGeneration

internal fun metadataFailureWithoutValidCandidate(
    metadataProblem: UpdateFailure?,
    validCandidateCount: Int,
): UpdateFailure? = metadataProblem?.takeIf { validCandidateCount == 0 }

internal class DeferredInstallerAction<T> {
    private var appResumed = false
    private var pendingAction: T? = null

    @Synchronized
    fun onResumed(): T? {
        appResumed = true
        return pendingAction.also { pendingAction = null }
    }

    @Synchronized
    fun onPaused() {
        appResumed = false
    }

    @Synchronized
    fun receive(action: T): T? {
        if (appResumed) return action
        pendingAction = action
        return null
    }
}

internal suspend fun writeApkBody(
    input: InputStream,
    partialFile: File,
    contentLength: Long?,
    maximumBytes: Long,
    sizeErrorResource: Int,
    onProgress: (Long, Long?) -> Unit,
): String {
    try {
        if (contentLength != null && contentLength > maximumBytes) {
            throw UpdateFailure(sizeErrorResource)
        }
        val digest = MessageDigest.getInstance("SHA-256")
        var totalBytes = 0L
        var lastReportedBytes = 0L
        input.use { source ->
            FileOutputStream(partialFile).use { output ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    currentCoroutineContext().ensureActive()
                    val read = source.read(buffer)
                    currentCoroutineContext().ensureActive()
                    if (read < 0) break
                    totalBytes += read
                    if (totalBytes > maximumBytes) throw UpdateFailure(sizeErrorResource)
                    output.write(buffer, 0, read)
                    digest.update(buffer, 0, read)
                    if (contentLength != null &&
                        (totalBytes - lastReportedBytes >= APK_PROGRESS_INTERVAL_BYTES || totalBytes == contentLength)
                    ) {
                        lastReportedBytes = totalBytes
                        onProgress(totalBytes, contentLength)
                    }
                }
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    } catch (failure: Exception) {
        partialFile.delete()
        throw failure
    }
}

private const val APK_PROGRESS_INTERVAL_BYTES = 512L * 1024

internal fun verifySha256(bytes: ByteArray, expectedSha256: String) {
    val actual = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    verifySha256Hex(actual, expectedSha256)
}

internal fun verifySha256Hex(actual: String, expectedSha256: String) {
    if (!actual.equals(expectedSha256, ignoreCase = true)) {
        throw UpdateFailure(R.string.app_update_error_checksum)
    }
}

internal fun readLimited(input: InputStream, maximumBytes: Int): ByteArray {
    val output = ByteArrayOutputStream(minOf(maximumBytes, 8 * 1024))
    val buffer = ByteArray(8 * 1024)
    var total = 0
    while (true) {
        val count = input.read(buffer)
        if (count < 0) break
        total += count
        if (total > maximumBytes) throw UpdateFailure(R.string.app_update_error_response_too_large)
        output.write(buffer, 0, count)
    }
    return output.toByteArray()
}

private fun JsonObject.requiredString(name: String): String =
    this[name]?.jsonPrimitive?.content ?: throw UpdateFailure(R.string.app_update_error_missing_field, listOf(name))

private const val ANDROID_APK_ASSET = "lognara-android.apk"
private val SHA256_REGEX = Regex("[0-9a-f]{64}")
private val STABLE_TAG_REGEX = Regex("android-\\d{8}-\\d{6}-[0-9a-fA-F]{12}")
private val DEV_TAG_REGEX = Regex("android-dev-\\d{8}-\\d{6}-[0-9a-fA-F]{12}")

package com.lognara.client.data

import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.nio.file.Files
import com.lognara.client.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReleaseUpdateLogicTest {
    @Test
    fun stableChannelSkipsDevDraftAndNonAndroidTags() {
        val eligible = releasesForChannel(
            listOf(
                release("android-20261118-120000-aaaaaaaaaaaa"),
                release("android-dev-20261118-130000-bbbbbbbbbbbb"),
                release("android-20261118-140000-cccccccccccc", draft = true),
                release("v2026.11.18"),
            ),
            UpdateChannel.Stable,
        )

        assertEquals(listOf("android-20261118-120000-aaaaaaaaaaaa"), eligible.map { it.tagName })
    }

    @Test
    fun devChannelSelectsHighestVersionAcrossStableAndDevReleases() {
        val stable = availableUpdate("android-20261118-120000-aaaaaaaaaaaa", "stable", 1001)
        val devOld = availableUpdate("android-dev-20261118-130000-bbbbbbbbbbbb", "dev", 1003)
        val devNew = availableUpdate("android-dev-20261118-140000-cccccccccccc", "dev", 1005)
        val eligible = releasesForChannel(
            listOf(stable.release, devOld.release, devNew.release),
            UpdateChannel.Dev,
        )
        val candidates = eligible.map { release -> listOf(stable, devOld, devNew).first { it.release == release } }

        assertEquals(devNew, chooseNewestUpdate(candidates, installedVersionCode = 1000))
    }

    @Test
    fun parsesVersionManifest() {
        val manifest = parseReleaseManifest(
            """{"versionCode":1234,"versionName":"2026.11.18.1400-dev","channel":"dev","commit":"abcdef012345","apk":"lognara-android.apk","sha256":"${"a".repeat(64)}"}""",
        )

        assertEquals(1234, manifest.versionCode)
        assertEquals("2026.11.18.1400-dev", manifest.versionName)
        assertEquals(UpdateChannel.Dev, manifest.channel)
        assertEquals("abcdef012345", manifest.commit)
        assertEquals("a".repeat(64), manifest.sha256)
    }

    @Test
    fun rejectsChecksumMismatch() {
        val failure = runCatching { verifySha256("apk".toByteArray(), "0".repeat(64)) }.exceptionOrNull()

        assertTrue(failure is UpdateFailure)
        assertEquals(R.string.app_update_error_checksum, (failure as UpdateFailure).messageResource)
    }

    @Test
    fun enforcesBodySizeLimitWithoutContentLength() {
        val failure = runCatching { readLimited(ByteArrayInputStream(ByteArray(5)), maximumBytes = 4) }.exceptionOrNull()

        assertTrue(failure is UpdateFailure)
    }

    @Test
    fun equalOrLowerVersionCodeDoesNotProduceUpdate() {
        val candidate = availableUpdate("android-20261118-120000-aaaaaaaaaaaa", "stable", 1000)
        val lowerCandidate = availableUpdate("android-20261118-130000-bbbbbbbbbbbb", "stable", 999)

        assertNull(chooseNewestUpdate(listOf(candidate), installedVersionCode = 1000))
        assertNull(chooseNewestUpdate(listOf(lowerCandidate), installedVersionCode = 1000))
    }

    @Test
    fun stableChannelAcceptsOnlyStableManifestsAndDevAcceptsBoth() {
        assertTrue(isManifestChannelAllowed(UpdateChannel.Stable, UpdateChannel.Stable))
        assertFalse(isManifestChannelAllowed(UpdateChannel.Dev, UpdateChannel.Stable))
        assertTrue(isManifestChannelAllowed(UpdateChannel.Stable, UpdateChannel.Dev))
        assertTrue(isManifestChannelAllowed(UpdateChannel.Dev, UpdateChannel.Dev))
    }

    @Test
    fun oldChannelCheckCannotUpdateCurrentState() {
        assertFalse(isCurrentUpdateRequest(UpdateChannel.Stable, UpdateChannel.Dev, 1, 2))
        assertFalse(isCurrentUpdateRequest(UpdateChannel.Dev, UpdateChannel.Dev, 1, 2))
        assertTrue(isCurrentUpdateRequest(UpdateChannel.Dev, UpdateChannel.Dev, 2, 2))
    }

    @Test
    fun validCandidateSuppressesMetadataErrorEvenWhenItIsNotNewer() {
        val metadataProblem = UpdateFailure(R.string.app_update_error_manifest)

        assertNull(metadataFailureWithoutValidCandidate(metadataProblem, validCandidateCount = 1))
        assertEquals(metadataProblem, metadataFailureWithoutValidCandidate(metadataProblem, validCandidateCount = 0))
    }

    @Test
    fun installerConfirmationWaitsUntilAppResumes() {
        val deferred = DeferredInstallerAction<String>()

        assertNull(deferred.receive("confirm"))
        assertEquals("confirm", deferred.onResumed())
        assertNull(deferred.onResumed())
        deferred.onPaused()
        assertNull(deferred.receive("confirm-again"))
        assertEquals("confirm-again", deferred.onResumed())
    }

    @Test
    fun startupCacheCleanupRemovesUpdateFilesAndKeepsDirectory() {
        val directory = Files.createTempDirectory("android-update-cache-test").toFile()
        try {
            val partialFile = File(directory, "lognara-android.apk.part")
            val nestedFile = File(directory, "temporary/nested.apk")
            partialFile.writeText("partial")
            nestedFile.parentFile!!.mkdirs()
            nestedFile.writeText("partial")

            clearUpdateCache(directory)

            assertTrue(directory.isDirectory)
            assertEquals(emptyList<File>(), directory.listFiles()?.toList())
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun cancellationDuringBodyReadDeletesPartialFileBeforeWritingReadBytes() = runBlocking {
        val directory = Files.createTempDirectory("android-partial-download-test").toFile()
        val partialFile = File(directory, "lognara-android.apk.part")
        val downloadJob = Job()
        var sent = false
        val input = object : InputStream() {
            override fun read(): Int = -1

            override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
                if (sent) return -1
                sent = true
                buffer[offset] = 42
                downloadJob.cancel()
                return 1
            }
        }

        try {
            val task = CoroutineScope(downloadJob + Dispatchers.IO).async {
                writeApkBody(input, partialFile, 1, 100, R.string.app_update_error_apk_too_large) { _, _ -> }
            }

            val failure = runCatching { task.await() }.exceptionOrNull()

            assertTrue(failure is java.util.concurrent.CancellationException)
            assertFalse(partialFile.exists())
        } finally {
            directory.deleteRecursively()
        }
    }

    private fun release(tag: String, draft: Boolean = false) = GithubRelease(
        tagName = tag,
        draft = draft,
        createdAt = tag,
        assets = emptyList(),
    )

    private fun availableUpdate(tag: String, channel: String, versionCode: Int): AvailableUpdate {
        val release = GithubRelease(
            tagName = tag,
            draft = false,
            createdAt = tag,
            assets = listOf(
                ReleaseAsset("version.json", "https://example.test/version.json"),
                ReleaseAsset("lognara-android.apk", "https://example.test/lognara-android.apk"),
            ),
        )
        return AvailableUpdate(
            release = release,
            manifest = ReleaseManifest(
                versionCode = versionCode,
                versionName = versionCode.toString(),
                channel = UpdateChannel.from(channel),
                commit = "abcdef012345",
                apk = "lognara-android.apk",
                sha256 = "a".repeat(64),
            ),
            apkAsset = release.assets.last(),
        )
    }
}

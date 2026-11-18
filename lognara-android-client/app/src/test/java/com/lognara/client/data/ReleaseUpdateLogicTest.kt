package com.lognara.client.data

import java.io.ByteArrayInputStream
import com.lognara.client.R
import org.junit.Assert.assertEquals
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

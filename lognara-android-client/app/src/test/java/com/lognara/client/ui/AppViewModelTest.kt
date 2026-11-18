package com.lognara.client.ui

import com.lognara.client.data.AvailableUpdate
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.GithubRelease
import com.lognara.client.data.ReleaseAsset
import com.lognara.client.data.ReleaseManifest
import com.lognara.client.data.UpdateChannel
import org.junit.Assert.assertFalse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AppViewModelTest {
    @Test
    fun unauthorizedResponseFromOldSessionDoesNotMatchCurrentSession() {
        val oldSession = DeviceSession("https://dock.example.com", "old-token", "device-1", "alice")
        val newSession = DeviceSession("https://dock.example.com", "new-token", "device-1", "alice")

        assertFalse(isCurrentUnauthorizedSession(oldSession, newSession))
        assertTrue(isCurrentUnauthorizedSession(newSession, newSession))
        assertFalse(isCurrentUnauthorizedSession(oldSession, null))
    }

    @Test
    fun installerTimeoutKeepsUpdateAvailableForRetry() {
        val release = GithubRelease(
            tagName = "android-20261118-120000-aaaaaaaaaaaa",
            draft = false,
            createdAt = "2026-11-18T12:00:00Z",
            assets = emptyList(),
        )
        val update = AvailableUpdate(
            release = release,
            manifest = ReleaseManifest(1234, "2026.11.18.1200", UpdateChannel.Stable, "abcdef", "lognara-android.apk", "a".repeat(64)),
            apkAsset = ReleaseAsset("lognara-android.apk", "https://example.test/lognara-android.apk"),
        )
        val state = AppUpdateUiState(phase = UpdatePhase.Installing, update = update)

        val timedOut = installTimeoutState(state, "Результат установки не получен")

        assertEquals(UpdatePhase.Available, timedOut.phase)
        assertEquals(update, timedOut.update)
        assertEquals("Результат установки не получен", timedOut.error)
    }
}

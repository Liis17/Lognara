package com.lognara.client.ui

import com.lognara.client.data.DeviceSession
import org.junit.Assert.assertFalse
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
}

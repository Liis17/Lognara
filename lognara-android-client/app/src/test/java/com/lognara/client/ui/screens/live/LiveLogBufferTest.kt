package com.lognara.client.ui.screens.live

import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LiveLogBufferTest {
    @Test
    fun deduplicatesByEventIdAndKeepsNewestFiveHundredRows() {
        val buffer = LiveLogBuffer()
        val batch = (0..LIVE_BUFFER_LIMIT).map(::event)

        val snapshot = buffer.append(batch + event(500))

        assertEquals(LIVE_BUFFER_LIMIT, snapshot.events.size)
        assertEquals("event-500", snapshot.events.first().id)
        assertFalse(snapshot.events.any { it.id == "event-0" })
        assertEquals(1, snapshot.events.count { it.id == "event-500" })
    }

    @Test
    fun pauseBuffersEventsAndResumePlacesThemBeforeVisibleRows() {
        val buffer = LiveLogBuffer()
        buffer.append(listOf(event(1)))
        buffer.setPaused(true)

        val paused = buffer.append(listOf(event(2), event(3)))

        assertTrue(paused.paused)
        assertEquals("event-1", paused.events.single().id)
        assertEquals(2, paused.pendingCount)

        val resumed = buffer.setPaused(false)

        assertFalse(resumed.paused)
        assertEquals(listOf("event-3", "event-2", "event-1"), resumed.events.map { it.id })
        assertEquals(0, resumed.pendingCount)
    }

    private fun event(id: Int) = LogEvent(
        id = "event-$id",
        timestamp = "2026-11-17T12:00:00Z",
        ingested_at = "2026-11-17T12:00:00Z",
        server = "node-1",
        backend = "http",
        service = "api",
        level = LogLevel.INFO,
        message = "message $id",
    )
}

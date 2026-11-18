package com.lognara.client.ui.screens.live

import com.lognara.client.data.LogLevel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class LiveEventParserTest {
    @Test
    fun parsesLogBatchAndControlEvents() {
        val event = """[{"id":"event-1","timestamp":"2026-11-17T12:00:00Z","ingested_at":"2026-11-17T12:00:00Z","environment":null,"server":"node-1","backend":"http","service":"api","service_instance":null,"action":null,"level":"error","message":"failed","trace_id":null,"span_id":null,"parent_span_id":null,"request_id":null,"attributes":{}}]"""

        val logs = parseLiveEvent("logs", event) as ParsedLiveEvent.Logs

        assertEquals(LogLevel.ERROR, logs.events.single().level)
        assertEquals(7L, (parseLiveEvent("lagged", """{"skipped_batches":7}""") as ParsedLiveEvent.Lagged).skippedBatches)
        assertEquals(ParsedLiveEvent.CoreError, parseLiveEvent("core_error", """{"message":"offline"}"""))
        assertEquals(ParsedLiveEvent.CoreOk, parseLiveEvent("core_ok", "{}"))
        assertTrue(parseLiveEvent("logs", "not json") is ParsedLiveEvent.Invalid)
    }
}

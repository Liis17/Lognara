package com.lognara.client.ui.screens.logs

import com.lognara.client.data.LogLevel
import com.lognara.client.data.SummaryRequest
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

class LogsViewModelTest {
    @Test
    fun buildsSummaryRequestWithTextLevelsAndFiftyEventPage() {
        val request = buildSummaryRequest(
            window = TimeWindow("2026-11-17T11:00:00Z", "2026-11-17T12:00:00Z"),
            range = DataRange.OneHour,
            query = "timeout",
            service = "api",
            levels = setOf(LogLevel.ERROR, LogLevel.WARN),
        )
        val body = apiJson.encodeToJsonElement(SummaryRequest.serializer(), request).jsonObject

        assertEquals("timeout", request.text?.query)
        assertEquals(mapOf("level" to listOf("error", "warn"), "service" to listOf("api")), request.filters)
        assertEquals(60L, request.interval_seconds)
        assertEquals(50, request.search_limit)
        assertEquals(
            setOf("from", "to", "filters", "text", "interval_seconds", "search_limit"),
            body.keys,
        )
        assertNotNull(body["text"])
    }
}

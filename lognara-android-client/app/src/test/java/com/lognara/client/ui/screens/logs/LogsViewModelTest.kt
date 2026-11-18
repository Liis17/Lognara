package com.lognara.client.ui.screens.logs

import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SearchRequest
import com.lognara.client.data.SearchResponse
import com.lognara.client.data.SummaryResponse
import com.lognara.client.data.DurationStats
import com.lognara.client.data.LevelCounts
import com.lognara.client.data.SummaryRequest
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.TimeWindow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withContext
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import okhttp3.OkHttpClient
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.TimeUnit

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

    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun cursorPageUsesTheWindowFiltersAndTextFromSummaryRequest() = runTest {
        Dispatchers.setMain(UnconfinedTestDispatcher(testScheduler))
        val server = MockWebServer()
        server.start()
        try {
            val asOf = "2026-11-17T12:00:00Z"
            server.enqueue(jsonResponse(apiJson.encodeToString(summaryResponse(asOf, null))))
            server.enqueue(jsonResponse(apiJson.encodeToString(summaryResponse(asOf, "next-cursor"))))
            server.enqueue(jsonResponse(apiJson.encodeToString(SearchResponse(emptyList(), null, asOf))))

            val session = DeviceSession(server.url("/").toString().trimEnd('/'), "lgd_test", "device-1", "alice")
            val viewModel = LogsViewModel(LognaraApi(OkHttpClient()), session)

            val initialContent = async { viewModel.state.first { it.result is DataLoadState.Content } }
            withContext(Dispatchers.IO) { server.takeRequest(5, TimeUnit.SECONDS) }
            initialContent.await()

            viewModel.search(" timeout ", " api ")
            val summaryContent = async { viewModel.state.first { it.result is DataLoadState.Content } }
            val summaryHttpRequest = requireNotNull(withContext(Dispatchers.IO) { server.takeRequest(5, TimeUnit.SECONDS) })
            summaryContent.await()

            viewModel.loadNextPage()
            val nextPageContent = async {
                viewModel.state.first {
                    val data = (it.result as? DataLoadState.Content)?.value
                    data?.nextCursor == null && !it.loadingMore
                }
            }
            val pageHttpRequest = requireNotNull(withContext(Dispatchers.IO) { server.takeRequest(5, TimeUnit.SECONDS) })
            nextPageContent.await()

            val summary = apiJson.decodeFromString<SummaryRequest>(summaryHttpRequest.body!!.utf8())
            val page = apiJson.decodeFromString<SearchRequest>(pageHttpRequest.body!!.utf8())
            assertTrue(summaryHttpRequest.requestLine.startsWith("POST /api/stats/summary "))
            assertTrue(pageHttpRequest.requestLine.startsWith("POST /api/logs/search "))
            assertEquals(summary.from, page.from)
            assertEquals(summary.to, page.to)
            assertEquals(summary.filters, page.filters)
            assertEquals(summary.text, page.text)
            assertEquals("next-cursor", page.cursor)
        } finally {
            server.close()
            Dispatchers.resetMain()
        }
    }

    private fun summaryResponse(asOf: String, cursor: String?): SummaryResponse = SummaryResponse(
        as_of = asOf,
        total = 0,
        buckets = emptyList(),
        levels = LevelCounts(0, 0, 0, 0, 0, 0, 0),
        error_services = emptyList(),
        duration = DurationStats(measured = 0, coverage = 0.0, method = "test"),
        actions = emptyList(),
        page = cursor?.let { SearchResponse(emptyList(), it, asOf) },
    )

    private fun jsonResponse(body: String) = MockResponse.Builder()
        .addHeader("Content-Type", "application/json")
        .body(body)
        .build()
}

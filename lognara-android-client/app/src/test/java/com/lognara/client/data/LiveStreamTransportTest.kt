package com.lognara.client.data

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import com.lognara.client.ui.screens.live.LiveStatus
import com.lognara.client.ui.screens.live.LiveStreamController

class LiveStreamTransportTest {
    private lateinit var server: MockWebServer
    private lateinit var api: LognaraApi
    private lateinit var session: DeviceSession

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        api = LognaraApi(OkHttpClient.Builder().build())
        session = DeviceSession(server.url("/").toString().trimEnd('/'), "lgd_test", "device-1", "alice")
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun initialStreamUnauthorizedNotifiesLogoutWithoutSessionProbe() {
        server.enqueue(MockResponse.Builder().code(401).build())
        val unauthorized = AtomicInteger()
        val sessionChecks = AtomicInteger()
        api.onUnauthorized = { unauthorized.incrementAndGet() }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val disconnected = CountDownLatch(1)
        val controller = controller(scope, sessionChecks) {
            if (it == LiveStatus.Disconnected) disconnected.countDown()
        }

        controller.start()
        val request = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        assertEquals("GET", request.method)
        assertEquals("/api/logs/stream", request.url.encodedPath)
        assertEquals("Bearer lgd_test", request.headers["Authorization"])
        assertTrue(disconnected.await(5, TimeUnit.SECONDS))
        assertEquals(1, unauthorized.get())
        assertEquals(0, sessionChecks.get())

        controller.stop()
        scope.cancel()
    }

    @Test
    fun closedStreamChecksSessionAndLogsOutOnUnauthorized() {
        server.enqueue(closedStream())
        server.enqueue(MockResponse.Builder().code(401).build())
        val unauthorized = AtomicInteger()
        val sessionChecks = AtomicInteger()
        api.onUnauthorized = { unauthorized.incrementAndGet() }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val disconnected = CountDownLatch(1)
        val controller = controller(scope, sessionChecks) {
            if (it == LiveStatus.Disconnected) disconnected.countDown()
        }

        controller.start()
        val streamRequest = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        val sessionRequest = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        assertEquals("/api/logs/stream", streamRequest.url.encodedPath)
        assertEquals("/api/me", sessionRequest.url.encodedPath)
        assertTrue(disconnected.await(5, TimeUnit.SECONDS))
        assertEquals(1, unauthorized.get())
        assertEquals(1, sessionChecks.get())

        controller.stop()
        scope.cancel()
    }

    @Test
    fun closedStreamWithValidSessionReconnects() {
        server.enqueue(closedStream())
        server.enqueue(MockResponse.Builder().body("""{"username":"alice","passkeys":0}""").build())
        server.enqueue(delayedStream())
        val sessionChecks = AtomicInteger()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val controller = controller(scope, sessionChecks) { }

        controller.start()
        val firstStream = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        val sessionRequest = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        val secondStream = checkNotNull(server.takeRequest(5, TimeUnit.SECONDS))
        assertEquals("/api/logs/stream", firstStream.url.encodedPath)
        assertEquals("/api/me", sessionRequest.url.encodedPath)
        assertEquals("/api/logs/stream", secondStream.url.encodedPath)
        assertEquals(1, sessionChecks.get())

        controller.stop()
        scope.cancel()
    }

    private fun controller(
        scope: CoroutineScope,
        sessionChecks: AtomicInteger,
        onStatus: (LiveStatus) -> Unit,
    ) = LiveStreamController(
        scope = scope,
        openStream = { listener -> api.openLogsStream(session, listener) },
        checkSession = {
            check(sessionChecks.incrementAndGet() > 0)
            check(api.getMe(session).username == "alice")
        },
        onStatus = onStatus,
        onEvent = { _, _ -> },
    )

    private fun closedStream() = MockResponse.Builder()
        .addHeader("Content-Type", "text/event-stream")
        .body("")
        .build()

    private fun delayedStream() = MockResponse.Builder()
        .addHeader("Content-Type", "text/event-stream")
        .body(":\n\n")
        .bodyDelay(30, TimeUnit.SECONDS)
        .build()
}

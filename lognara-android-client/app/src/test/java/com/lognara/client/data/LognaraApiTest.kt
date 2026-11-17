package com.lognara.client.data

import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class LognaraApiTest {
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
    fun unauthorizedResponseCallsCommonHandler() = runBlocking {
        server.enqueue(MockResponse.Builder().code(401).body("""{"error":"unauthorized"}""").build())
        var loggedOut = false
        api.onUnauthorized = { loggedOut = true }

        val failure = runCatching { api.authenticated(session, "GET", DeviceApiRoutes.ME) }.exceptionOrNull()

        assertTrue(failure is ApiException.Unauthorized)
        assertTrue(loggedOut)
        assertEquals("Bearer lgd_test", server.takeRequest().headers["Authorization"])
    }

    @Test
    fun redirectsAreRejectedWithoutFollowingThem() = runBlocking {
        server.enqueue(MockResponse.Builder().code(302).setHeader("Location", "/api/me").build())

        val failure = runCatching { api.authenticated(session, "GET", DeviceApiRoutes.ME) }.exceptionOrNull()

        assertTrue(failure is ApiException.Redirect)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun chunkedResponseIsLimitedEvenWithoutContentLength() = runBlocking {
        server.enqueue(MockResponse.Builder().chunkedBody("x".repeat(4 * 1024 * 1024 + 1), 16 * 1024).build())

        val failure = runCatching { api.authenticated(session, "GET", DeviceApiRoutes.INFRASTRUCTURE) }.exceptionOrNull()

        assertEquals(ApiException.TooLarge, failure)
    }

    @Test
    fun authResponseRetainsOneMebibyteLimit() = runBlocking {
        server.enqueue(MockResponse.Builder().body("x".repeat(1024 * 1024 + 1)).build())

        val failure = runCatching { api.getMe(session) }.exceptionOrNull()

        assertEquals(ApiException.TooLarge, failure)
    }

    @Test
    fun cancellingCoroutineCancelsTheOkHttpCall() = runBlocking {
        server.enqueue(MockResponse.Builder().body("{\"ok\":true}").bodyDelay(10, TimeUnit.SECONDS).build())
        val request = launch { api.authenticated(session, "GET", DeviceApiRoutes.ME) }

        val observed = withContext(Dispatchers.IO) { server.takeRequest(3, TimeUnit.SECONDS) }
        assertNotNull(observed)
        request.cancelAndJoin()
        assertTrue(request.isCancelled)
    }
}

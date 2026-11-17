package com.lognara.client.data

import java.io.IOException
import java.net.SocketTimeoutException
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.SerializationException
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.JsonElement
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

class LognaraApi(
    client: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(40, TimeUnit.SECONDS)
        .followRedirects(false)
        .followSslRedirects(false)
        .cache(null)
        .build(),
) {
    private val client = client.newBuilder().followRedirects(false).followSslRedirects(false).cache(null).build()
    @Volatile
    var onUnauthorized: (() -> Unit)? = null

    suspend fun pair(payload: PairPayload, deviceName: String): PairResult {
        val jsonBody = apiJson.encodeToString(PairRequest(payload.code, deviceName))
        return try {
            val request = Request.Builder()
                .url("${payload.url}/api/devices/pair")
                .header("Cache-Control", "no-store")
                .post(jsonBody.toRequestBody(JSON_MEDIA_TYPE))
                .build()
            val response = execute(request, AUTH_RESPONSE_BODY_BYTES)
            parsePairResponse(response.status, response.body)
        } catch (_: ApiException.TooLarge) {
            PairResult.Failure(PairError.InvalidResponse)
        } catch (_: SocketTimeoutException) {
            PairResult.Failure(PairError.Timeout)
        } catch (_: IllegalArgumentException) {
            PairResult.Failure(PairError.Unknown)
        } catch (_: IOException) {
            PairResult.Failure(PairError.Network)
        }
    }

    suspend fun getMe(session: DeviceSession): MeResponse {
        val body = authenticatedResponse(session, "GET", DeviceApiRoutes.ME, maxResponseBytes = AUTH_RESPONSE_BODY_BYTES).body
        return decodeJson(body)
    }

    suspend fun disconnect(session: DeviceSession) {
        val response = authenticatedResponse(session, "DELETE", DeviceApiRoutes.DISCONNECT, maxResponseBytes = AUTH_RESPONSE_BODY_BYTES)
        if (response.status != 204) throw ApiException.Server(response.status, decodeError(response.body))
    }

    fun openLogsStream(session: DeviceSession, listener: LiveStreamListener): LiveConnection {
        val request = authenticatedRequest(session, "GET", DeviceApiRoutes.LIVE)
            .newBuilder()
            .header("Accept", "text/event-stream")
            .build()
        val source = EventSources.createFactory(client).newEventSource(request, object : EventSourceListener() {
            override fun onOpen(eventSource: EventSource, response: Response) = listener.onOpen()

            override fun onEvent(eventSource: EventSource, id: String?, type: String?, data: String) =
                listener.onEvent(type, data)

            override fun onClosed(eventSource: EventSource) = listener.onClosed()

            override fun onFailure(eventSource: EventSource, t: Throwable?, response: Response?) {
                val status = response?.code
                if (status == 401) onUnauthorized?.invoke()
                listener.onFailure(status)
            }
        })
        return LiveConnection { source.cancel() }
    }

    suspend inline fun <reified T> authenticatedJson(
        session: DeviceSession,
        method: String,
        path: String,
        body: JsonElement? = null,
    ): T {
        val responseBody = authenticated(session, method, path, body)
        return try {
            apiJson.decodeFromString<T>(responseBody)
        } catch (_: SerializationException) {
            throw ApiException.InvalidResponse
        } catch (_: IllegalArgumentException) {
            throw ApiException.InvalidResponse
        }
    }

    suspend fun authenticated(
        session: DeviceSession,
        method: String,
        path: String,
        body: JsonElement? = null,
    ): String = authenticatedResponse(session, method, path, body).body

    private suspend fun authenticatedResponse(
        session: DeviceSession,
        method: String,
        path: String,
        body: JsonElement? = null,
        maxResponseBytes: Int = DATA_RESPONSE_BODY_BYTES,
    ): ApiHttpResponse {
        val request = authenticatedRequest(session, method, path, body)
        val response = execute(request, maxResponseBytes, skipRedirectAndUnauthorizedBodies = true)
        if (response.status in 300..399) throw ApiException.Redirect
        if (response.status == 401) {
            onUnauthorized?.invoke()
            throw ApiException.Unauthorized
        }
        if (response.status !in 200..299) throw ApiException.Server(response.status, decodeError(response.body))
        return response
    }

    private fun authenticatedRequest(
        session: DeviceSession,
        method: String,
        path: String,
        body: JsonElement? = null,
    ): Request {
        val builder = Request.Builder()
            .url("${session.serverUrl}$path")
            .header("Cache-Control", "no-store")
            .header("Authorization", "Bearer ${session.token}")
        return when (method) {
            "GET" -> builder.get().build()
            "POST" -> builder.post((body?.toString() ?: "").toRequestBody(JSON_MEDIA_TYPE)).build()
            "DELETE" -> builder.delete().build()
            else -> throw IllegalArgumentException("Unsupported device API method")
        }
    }

    private fun decodeError(body: String): String = try {
        apiJson.decodeFromString<ApiErrorBody>(body).error
    } catch (_: Exception) {
        ""
    }

    private inline fun <reified T> decodeJson(body: String): T = try {
        apiJson.decodeFromString<T>(body)
    } catch (_: SerializationException) {
        throw ApiException.InvalidResponse
    } catch (_: IllegalArgumentException) {
        throw ApiException.InvalidResponse
    }

    private fun readBody(response: Response, maxBytes: Int): String {
        val bytes = response.body.byteStream().readNBytes(maxBytes + 1)
        if (bytes.size > maxBytes) throw ApiException.TooLarge
        return String(bytes, StandardCharsets.UTF_8)
    }

    private suspend fun execute(
        request: Request,
        maxResponseBytes: Int,
        skipRedirectAndUnauthorizedBodies: Boolean = false,
    ): ApiHttpResponse = suspendCancellableCoroutine { continuation ->
        val call = client.newCall(request)
        continuation.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (continuation.isActive) continuation.resumeWithException(e)
            }

            override fun onResponse(call: Call, response: Response) {
                try {
                    val result = response.use {
                        val body = if (skipRedirectAndUnauthorizedBodies && (it.code == 401 || it.code in 300..399)) {
                            ""
                        } else {
                            readBody(it, maxResponseBytes)
                        }
                        ApiHttpResponse(it.code, body)
                    }
                    if (continuation.isActive) continuation.resume(result)
                } catch (exception: IOException) {
                    if (continuation.isActive) continuation.resumeWithException(exception)
                }
            }
        })
    }

    private data class ApiHttpResponse(val status: Int, val body: String)

    private companion object {
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
        const val AUTH_RESPONSE_BODY_BYTES = 1024 * 1024
        const val DATA_RESPONSE_BODY_BYTES = 4 * 1024 * 1024
    }
}

sealed class ApiException : IOException() {
    data object Unauthorized : ApiException()
    data object InvalidResponse : ApiException()
    data object Redirect : ApiException()
    data object TooLarge : ApiException()
    data class Server(val status: Int, val code: String) : ApiException()
}

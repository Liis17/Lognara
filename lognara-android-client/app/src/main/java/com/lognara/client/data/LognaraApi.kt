package com.lognara.client.data

import java.io.IOException
import java.net.SocketTimeoutException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

class LognaraApi(
    private val client: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(40, TimeUnit.SECONDS)
        .followRedirects(false)
        .followSslRedirects(false)
        .cache(null)
        .build(),
) {
    suspend fun pair(payload: PairPayload, deviceName: String): PairResult = withContext(Dispatchers.IO) {
        val jsonBody = apiJson.encodeToString(PairRequest(payload.code, deviceName))
        try {
            val request = Request.Builder()
                .url("${payload.url}/api/devices/pair")
                .header("Cache-Control", "no-store")
                .post(jsonBody.toRequestBody(JSON_MEDIA_TYPE))
                .build()
            client.newCall(request).execute().use { response ->
                parsePairResponse(response.code, readBody(response))
            }
        } catch (_: SocketTimeoutException) {
            PairResult.Failure(PairError.Timeout)
        } catch (_: IllegalArgumentException) {
            PairResult.Failure(PairError.Unknown)
        } catch (_: IOException) {
            PairResult.Failure(PairError.Network)
        }
    }

    suspend fun getMe(session: DeviceSession): MeResponse = withContext(Dispatchers.IO) {
        val request = authenticatedRequest(session, "/api/me").get().build()
        client.newCall(request).execute().use { response ->
            if (response.code in 300..399) throw ApiException.Redirect
            if (response.code == 401) throw ApiException.Unauthorized
            val body = readBody(response)
            if (!response.isSuccessful) throw ApiException.Server(response.code, decodeError(body))
            try {
                apiJson.decodeFromString<MeResponse>(body)
            } catch (_: Exception) {
                throw ApiException.InvalidResponse
            }
        }
    }

    suspend fun disconnect(session: DeviceSession) = withContext(Dispatchers.IO) {
        val request = authenticatedRequest(session, "/api/devices/current").delete().build()
        client.newCall(request).execute().use { response ->
            if (response.code in 300..399) throw ApiException.Redirect
            if (response.code == 401) throw ApiException.Unauthorized
            if (response.code != 204) {
                throw ApiException.Server(response.code, decodeError(readBody(response)))
            }
        }
    }

    private fun authenticatedRequest(session: DeviceSession, path: String): Request.Builder =
        Request.Builder()
            .url("${session.serverUrl}$path")
            .header("Cache-Control", "no-store")
            .header("Authorization", "Bearer ${session.token}")

    private fun decodeError(body: String): String = try {
        apiJson.decodeFromString<ApiErrorBody>(body).error
    } catch (_: Exception) {
        ""
    }

    private fun readBody(response: Response): String = response.peekBody(MAX_RESPONSE_BODY_BYTES).string()

    private companion object {
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
        const val MAX_RESPONSE_BODY_BYTES = 1024L * 1024L
    }
}

sealed class ApiException : IOException() {
    data object Unauthorized : ApiException()
    data object InvalidResponse : ApiException()
    data object Redirect : ApiException()
    data class Server(val status: Int, val code: String) : ApiException()
}

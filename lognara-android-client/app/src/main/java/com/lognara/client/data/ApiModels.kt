package com.lognara.client.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

@Serializable
data class PairRequest(val code: String, val name: String) {
    override fun toString(): String = "PairRequest(code=***, name=$name)"
}

@Serializable
data class PairResponse(
    val token: String,
    val device_id: String,
    val username: String,
) {
    override fun toString(): String = "PairResponse(token=***, device_id=$device_id, username=$username)"
}

@Serializable
data class ApiErrorBody(val error: String)

@Serializable
data class MeResponse(val username: String, val passkeys: Int)

data class DeviceSession(
    val serverUrl: String,
    val token: String,
    val deviceId: String,
    val username: String,
) {
    override fun toString(): String = "DeviceSession(serverUrl=$serverUrl, token=***, deviceId=$deviceId, username=$username)"
}

enum class PairError {
    InvalidRequest,
    InvalidCode,
    DeviceLimit,
    TooManyAttempts,
    OutdatedDock,
    Network,
    Timeout,
    Redirect,
    InvalidResponse,
    Storage,
    Unknown,
}

sealed interface PairResult {
    data class Success(val response: PairResponse) : PairResult
    data class Failure(val error: PairError) : PairResult
}

val apiJson = Json { ignoreUnknownKeys = true }

fun parsePairResponse(status: Int, body: String): PairResult {
    if (status in 300..399) return PairResult.Failure(PairError.Redirect)
    if (status == 201) {
        val response = try {
            apiJson.decodeFromString<PairResponse>(body)
        } catch (_: SerializationException) {
            return PairResult.Failure(PairError.InvalidResponse)
        } catch (_: IllegalArgumentException) {
            return PairResult.Failure(PairError.InvalidResponse)
        }
        if (!response.token.startsWith("lgd_") || response.device_id.isBlank() || response.username.isBlank()) {
            return PairResult.Failure(PairError.InvalidResponse)
        }
        return PairResult.Success(response)
    }

    val errorCode = try {
        apiJson.decodeFromString<ApiErrorBody>(body).error
    } catch (_: Exception) {
        ""
    }
    val error = when {
        status == 404 -> PairError.OutdatedDock
        status == 400 && errorCode == "invalid_request" -> PairError.InvalidRequest
        status == 401 && errorCode == "invalid_code" -> PairError.InvalidCode
        status == 409 && errorCode == "device_limit" -> PairError.DeviceLimit
        status == 429 && errorCode == "too_many_attempts" -> PairError.TooManyAttempts
        else -> PairError.Unknown
    }
    return PairResult.Failure(error)
}

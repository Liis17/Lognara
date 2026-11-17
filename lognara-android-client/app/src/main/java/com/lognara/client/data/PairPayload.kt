package com.lognara.client.data

import java.net.URI
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

@Serializable
data class PairPayload(
    val v: Int,
    val url: String,
    val code: String,
) {
    override fun toString(): String = "PairPayload(v=$v, url=$url, code=***)"
}

enum class PayloadIssue {
    InvalidBase64,
    InvalidJson,
    UnsupportedVersion,
    InvalidUrl,
    InsecureUrl,
    InvalidCode,
}

class PairPayloadException(val issue: PayloadIssue) : Exception()

private val payloadJson = Json { ignoreUnknownKeys = true }

fun decodePairPayload(payload: String, allowInsecureLocalhost: Boolean): PairPayload {
    val decoded = try {
        Base64.getUrlDecoder().decode(payload.trim())
    } catch (_: IllegalArgumentException) {
        throw PairPayloadException(PayloadIssue.InvalidBase64)
    }
    val parsed = try {
        payloadJson.decodeFromString<PairPayload>(decoded.toString(Charsets.UTF_8))
    } catch (_: SerializationException) {
        throw PairPayloadException(PayloadIssue.InvalidJson)
    } catch (_: IllegalArgumentException) {
        throw PairPayloadException(PayloadIssue.InvalidJson)
    }

    if (parsed.v != 1) throw PairPayloadException(PayloadIssue.UnsupportedVersion)
    validateServerUrl(parsed.url, allowInsecureLocalhost)

    val code = parsed.code
    if (!code.matches(Regex("[A-Za-z0-9_-]+={0,2}"))) {
        throw PairPayloadException(PayloadIssue.InvalidCode)
    }
    val codeBytes = try {
        Base64.getUrlDecoder().decode(code)
    } catch (_: IllegalArgumentException) {
        throw PairPayloadException(PayloadIssue.InvalidCode)
    }
    if (codeBytes.isEmpty()) throw PairPayloadException(PayloadIssue.InvalidCode)

    return parsed.copy(url = parsed.url.trimEnd('/'))
}

fun validateServerUrl(url: String, allowInsecureLocalhost: Boolean) {
    val httpUrl = url.toHttpUrlOrNull() ?: throw PairPayloadException(PayloadIssue.InvalidUrl)
    val uri = try {
        URI(url)
    } catch (_: Exception) {
        throw PairPayloadException(PayloadIssue.InvalidUrl)
    }
    if (httpUrl.port !in 1..65535 || httpUrl.host.isBlank() || uri.rawUserInfo != null || uri.rawQuery != null || uri.rawFragment != null) {
        throw PairPayloadException(PayloadIssue.InvalidUrl)
    }
    when (httpUrl.scheme) {
        "https" -> Unit
        "http" -> if (!allowInsecureLocalhost || httpUrl.host !in setOf("localhost", "127.0.0.1")) {
            throw PairPayloadException(PayloadIssue.InsecureUrl)
        }
        else -> throw PairPayloadException(PayloadIssue.InvalidUrl)
    }
}

fun buildDeviceName(manufacturer: String, model: String): String {
    val parts = listOf(manufacturer.trim(), model.trim()).filter(String::isNotEmpty)
    val name = parts.joinToString(" ")
    val length = name.codePointCount(0, name.length)
    return if (length <= 64) name else name.substring(0, name.offsetByCodePoints(0, 64))
}

package com.lognara.client.ui.screens.live

import com.lognara.client.data.LogEvent
import com.lognara.client.data.apiJson
import kotlinx.serialization.SerializationException
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

sealed interface ParsedLiveEvent {
    data class Logs(val events: List<LogEvent>) : ParsedLiveEvent
    data class Lagged(val skippedBatches: Long) : ParsedLiveEvent
    data object CoreError : ParsedLiveEvent
    data object CoreOk : ParsedLiveEvent
    data object Invalid : ParsedLiveEvent
}

fun parseLiveEvent(type: String?, data: String): ParsedLiveEvent? = when (type) {
    "logs" -> try {
        ParsedLiveEvent.Logs(apiJson.decodeFromString<List<LogEvent>>(data))
    } catch (_: SerializationException) {
        ParsedLiveEvent.Invalid
    } catch (_: IllegalArgumentException) {
        ParsedLiveEvent.Invalid
    }
    "lagged" -> try {
        val value = apiJson.parseToJsonElement(data).jsonObject["skipped_batches"]?.jsonPrimitive?.longOrNull ?: 0
        ParsedLiveEvent.Lagged(value)
    } catch (_: Exception) {
        ParsedLiveEvent.Invalid
    }
    "core_error" -> ParsedLiveEvent.CoreError
    "core_ok" -> ParsedLiveEvent.CoreOk
    else -> null
}

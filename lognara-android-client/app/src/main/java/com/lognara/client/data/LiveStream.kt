package com.lognara.client.data

class LiveStreamListener(
    val onOpen: () -> Unit,
    val onEvent: (type: String?, data: String) -> Unit,
    val onClosed: () -> Unit,
    val onFailure: (statusCode: Int?) -> Unit,
)

fun interface LiveConnection {
    fun cancel()
}

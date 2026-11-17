package com.lognara.client.ui.screens.live

import com.lognara.client.data.LogEvent

const val LIVE_BUFFER_LIMIT = 500

data class LiveBufferSnapshot(
    val events: List<LogEvent>,
    val paused: Boolean,
    val pendingCount: Int,
)

class LiveLogBuffer(private val capacity: Int = LIVE_BUFFER_LIMIT) {
    private val seen = linkedSetOf<String>()
    private var events = emptyList<LogEvent>()
    private var pending = emptyList<LogEvent>()
    private var paused = false

    fun append(batch: List<LogEvent>): LiveBufferSnapshot {
        val fresh = batch.filter { rememberId(it.id) }
        if (paused) {
            pending = (fresh.asReversed() + pending).take(capacity)
        } else {
            events = (fresh.asReversed() + events).take(capacity)
        }
        return snapshot()
    }

    fun setPaused(value: Boolean): LiveBufferSnapshot {
        if (paused == value) return snapshot()
        paused = value
        if (!paused) {
            events = (pending + events).take(capacity)
            pending = emptyList()
        }
        return snapshot()
    }

    fun snapshot(): LiveBufferSnapshot = LiveBufferSnapshot(events, paused, pending.size)

    private fun rememberId(id: String): Boolean {
        if (!seen.add(id)) return false
        while (seen.size > capacity * 2) seen.remove(seen.first())
        return true
    }
}

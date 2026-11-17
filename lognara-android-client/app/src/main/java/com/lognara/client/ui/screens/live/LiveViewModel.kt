package com.lognara.client.ui.screens.live

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

enum class LiveNotice { Lagged, CoreUnavailable, InvalidData }

data class LiveUiState(
    val status: LiveStatus = LiveStatus.Disconnected,
    val events: List<LogEvent> = emptyList(),
    val paused: Boolean = false,
    val pendingCount: Int = 0,
    val skippedBatches: Long = 0,
    val levels: Set<LogLevel> = LogLevel.entries.toSet(),
    val service: String = "",
    val notice: LiveNotice? = null,
)

class LiveViewModel(private val api: LognaraApi, private val session: DeviceSession) : ViewModel() {
    private val _state = MutableStateFlow(LiveUiState())
    val state = _state.asStateFlow()
    private val buffer = LiveLogBuffer()
    private val controller = LiveStreamController(
        scope = viewModelScope,
        openStream = { listener -> api.openLogsStream(session, listener) },
        checkSession = { api.getMe(session).let { } },
        onStatus = { status -> _state.update { it.copy(status = status) } },
        onEvent = ::handleEvent,
    )

    fun start() = controller.start()

    fun stop() = controller.stop()

    fun retry() {
        controller.stop()
        controller.start()
    }

    fun togglePaused() {
        updateBuffer(buffer.setPaused(!_state.value.paused))
    }

    fun setLevel(level: LogLevel, selected: Boolean) {
        _state.update { current ->
            current.copy(levels = if (selected) current.levels + level else current.levels - level)
        }
    }

    fun setService(service: String) {
        _state.update { it.copy(service = service) }
    }

    private fun handleEvent(type: String?, data: String) {
        when (val event = parseLiveEvent(type, data)) {
            is ParsedLiveEvent.Logs -> updateBuffer(buffer.append(event.events))
            is ParsedLiveEvent.Lagged -> _state.update {
                it.copy(notice = LiveNotice.Lagged, skippedBatches = event.skippedBatches)
            }
            ParsedLiveEvent.CoreError -> _state.update { it.copy(status = LiveStatus.CoreError, notice = LiveNotice.CoreUnavailable) }
            ParsedLiveEvent.CoreOk -> _state.update { it.copy(status = LiveStatus.Connected, notice = null) }
            ParsedLiveEvent.Invalid -> _state.update { it.copy(notice = LiveNotice.InvalidData) }
            null -> Unit
        }
    }

    private fun updateBuffer(snapshot: LiveBufferSnapshot) {
        _state.update {
            it.copy(events = snapshot.events, paused = snapshot.paused, pendingCount = snapshot.pendingCount)
        }
    }
}

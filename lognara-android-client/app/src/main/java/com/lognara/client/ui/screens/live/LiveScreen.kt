package com.lognara.client.ui.screens.live

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.screens.logs.LogEventRow
import com.lognara.client.ui.screens.logs.levelLabel

@Composable
fun LiveScreen(api: LognaraApi, session: DeviceSession, onOpenEvent: (LogEvent) -> Unit, onSettings: () -> Unit) {
    androidx.compose.foundation.layout.Column(Modifier.fillMaxSize()) {
        com.lognara.client.ui.components.ScreenHeader(
            title = stringResource(R.string.tab_live),
            actions = { com.lognara.client.ui.components.SettingsButton(onSettings) },
        )
        androidx.compose.foundation.layout.Box(Modifier.weight(1f)) {
    val viewModel: LiveViewModel = viewModel(key = "live-${session.deviceId}") {
        LiveViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    var foreground by remember(lifecycleOwner) {
        mutableStateOf(lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))
    }
    var service by rememberSaveable { mutableStateOf(state.service) }

    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, _ ->
            foreground = lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    DisposableEffect(viewModel) {
        onDispose { viewModel.stop() }
    }
    LaunchedEffect(foreground) {
        if (foreground) viewModel.start() else viewModel.stop()
    }

    val filteredEvents = remember(state.events, state.levels, state.service) {
        state.events.filter { event ->
            event.level in state.levels && (state.service.isBlank() || event.service.contains(state.service.trim(), ignoreCase = true))
        }
    }

    Column(Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 8.dp)) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                text = stringResource(statusLabel(state.status)),
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.weight(1f).padding(top = 12.dp),
            )
            Button(onClick = viewModel::togglePaused) {
                Text(stringResource(if (state.paused) R.string.live_resume else R.string.live_pause))
            }
        }
        if (state.paused) {
            Text(stringResource(R.string.live_pending_count, state.pendingCount), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (state.status == LiveStatus.Connecting) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (state.status == LiveStatus.Reconnecting || state.status == LiveStatus.Disconnected || state.status == LiveStatus.CoreError) {
            Text(stringResource(R.string.live_connection_error), color = MaterialTheme.colorScheme.error)
            TextButton(onClick = viewModel::retry) { Text(stringResource(R.string.retry)) }
        }
        state.notice?.let { notice ->
            val noticeText = if (notice == LiveNotice.Lagged) {
                stringResource(R.string.live_notice_lagged, state.skippedBatches)
            } else {
                stringResource(noticeLabel(notice))
            }
            Text(noticeText, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        OutlinedTextField(
            value = service,
            onValueChange = { service = it; viewModel.setService(it) },
            label = { Text(stringResource(R.string.live_service_filter)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        LazyRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            items(LogLevel.entries.toList()) { level ->
                FilterChip(
                    selected = level in state.levels,
                    onClick = { viewModel.setLevel(level, level !in state.levels) },
                    label = { Text(stringResource(levelLabel(level))) },
                )
            }
        }
        if (filteredEvents.isEmpty()) {
            val emptyText = if (state.status == LiveStatus.Connecting) R.string.live_connecting else R.string.live_empty
            Text(
                text = stringResource(emptyText),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.fillMaxWidth().padding(24.dp),
            )
        } else {
            LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                items(filteredEvents, key = { it.id }) { event ->
                    LogEventRow(event = event, onClick = { onOpenEvent(event) })
                }
            }
        }
    }
        }
    }
}

private fun statusLabel(status: LiveStatus): Int = when (status) {
    LiveStatus.Disconnected -> R.string.live_status_disconnected
    LiveStatus.Connecting -> R.string.live_status_connecting
    LiveStatus.Connected -> R.string.live_status_connected
    LiveStatus.Reconnecting -> R.string.live_status_reconnecting
    LiveStatus.CoreError -> R.string.live_status_core_error
}

private fun noticeLabel(notice: LiveNotice): Int = when (notice) {
    LiveNotice.Lagged -> R.string.live_notice_lagged
    LiveNotice.CoreUnavailable -> R.string.live_status_core_error
    LiveNotice.InvalidData -> R.string.live_notice_invalid
}

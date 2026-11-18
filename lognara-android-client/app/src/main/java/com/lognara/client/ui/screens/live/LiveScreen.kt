package com.lognara.client.ui.screens.live

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
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
import com.lognara.client.ui.components.ChipRow
import com.lognara.client.ui.components.FilterPill
import com.lognara.client.ui.components.LevelDot
import com.lognara.client.ui.components.LogEventRow as DesignLogEventRow
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.ScreenHeader
import com.lognara.client.ui.components.SearchField
import com.lognara.client.ui.components.SettingsButton
import com.lognara.client.ui.components.levelColor
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

@Composable
fun LiveScreen(api: LognaraApi, session: DeviceSession, onOpenEvent: (LogEvent) -> Unit, onSettings: () -> Unit) {
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
    val statusText = stringResource(liveStatusTitle(state.status, state.paused))
    val statusColor = liveStatusColor(state.status, state.paused)
    val latestTime = state.events.firstOrNull()?.timestamp?.let(::formatLiveTime)
        ?: stringResource(R.string.live_last_event_empty)
    val hasConnectionError = state.status == LiveStatus.Reconnecting ||
        state.status == LiveStatus.Disconnected || state.status == LiveStatus.CoreError

    Column(Modifier.fillMaxSize()) {
        ScreenHeader(
            title = stringResource(R.string.tab_live),
            subtitle = stringResource(R.string.live_subtitle),
            actions = { SettingsButton(onSettings) },
        )

        Column(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .padding(horizontal = 16.dp)
                .padding(bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            LognaraCard(
                contentPadding = PaddingValues(start = 16.dp, top = 12.dp, end = 12.dp, bottom = 12.dp),
                shape = RoundedCornerShape(14.dp),
                verticalArrangement = Arrangement.spacedBy(0.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    LiveStatusDot(statusColor)
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        Text(statusText, fontSize = 14.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
                        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(
                                stringResource(R.string.live_last_event),
                                fontSize = 12.sp,
                                color = LognaraPalette.TextMuted,
                                maxLines = 1,
                            )
                            Text(latestTime, style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1)
                        }
                    }
                    LivePauseButton(paused = state.paused, onClick = viewModel::togglePaused)
                }
                if (state.paused) {
                    Text(
                        stringResource(R.string.live_pending_count, state.pendingCount),
                        fontSize = 12.sp,
                        color = LognaraPalette.TextMuted,
                        modifier = Modifier.padding(start = 22.dp, top = 6.dp),
                    )
                }
            }

            if (state.status == LiveStatus.Connecting) {
                LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp))
            }

            val noticeText = state.notice?.let { notice ->
                if (notice == LiveNotice.Lagged) {
                    stringResource(R.string.live_notice_lagged, state.skippedBatches)
                } else {
                    stringResource(noticeLabel(notice))
                }
            }
            if (hasConnectionError || noticeText != null) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = noticeText ?: stringResource(R.string.live_connection_error),
                        color = if (hasConnectionError) LognaraSemanticColors.statusErr else LognaraPalette.TextMuted,
                        fontSize = 12.sp,
                        modifier = Modifier.weight(1f),
                    )
                    if (hasConnectionError) {
                        TextButton(onClick = viewModel::retry) { Text(stringResource(R.string.retry)) }
                    }
                }
            }

            SearchField(
                value = service,
                onValueChange = { service = it; viewModel.setService(it) },
                placeholder = stringResource(R.string.live_service_filter),
            )

            ChipRow(horizontalPadding = 0.dp) {
                LogLevel.entries.forEach { level ->
                    val enabled = level in state.levels
                    val filterDescription = stringResource(
                        R.string.live_filter_level_description,
                        level.name,
                        stringResource(if (enabled) R.string.live_filter_level_enabled else R.string.live_filter_level_disabled),
                    )
                    FilterPill(
                        text = level.name,
                        onClick = { viewModel.setLevel(level, !enabled) },
                        modifier = Modifier.semantics(mergeDescendants = true) {
                            contentDescription = filterDescription
                            selected = enabled
                        },
                        selected = false,
                        mono = true,
                        textColor = if (enabled) LognaraPalette.TextSecondary else LognaraPalette.TextMuted,
                        leading = { LevelDot(color = levelColor(level), filled = enabled) },
                    )
                }
            }

            LognaraCard(
                modifier = Modifier.weight(1f),
                contentPadding = PaddingValues(0.dp),
                shape = RoundedCornerShape(20.dp),
                verticalArrangement = Arrangement.spacedBy(0.dp),
            ) {
                if (filteredEvents.isEmpty()) {
                    val emptyText = if (state.status == LiveStatus.Connecting) R.string.live_connecting else R.string.live_empty
                    Text(
                        text = stringResource(emptyText),
                        color = LognaraPalette.TextMuted,
                        modifier = Modifier.fillMaxWidth().padding(24.dp),
                    )
                } else {
                    LazyColumn(Modifier.fillMaxSize()) {
                        itemsIndexed(filteredEvents, key = { _, event -> event.id }) { index, event ->
                            DesignLogEventRow(
                                level = event.level,
                                time = formatLiveTime(event.timestamp),
                                service = event.service,
                                server = event.server,
                                message = event.message,
                                onClick = { onOpenEvent(event) },
                                highlighted = index == 0,
                            )
                            if (index < filteredEvents.lastIndex) RowDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LiveStatusDot(color: Color) {
    Box(
        modifier = Modifier.size(18.dp).clip(CircleShape).background(color.copy(alpha = 0.12f)),
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(10.dp).clip(CircleShape).background(color))
    }
}

@Composable
private fun LivePauseButton(paused: Boolean, onClick: () -> Unit) {
    val text = stringResource(if (paused) R.string.live_resume else R.string.live_pause)
    Row(
        modifier = Modifier
            .height(44.dp)
            .clip(RoundedCornerShape(999.dp))
            .background(LognaraPalette.SurfaceRaised)
            .border(1.dp, LognaraPalette.BorderStrong, RoundedCornerShape(999.dp))
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp)
            .semantics(mergeDescendants = true) { contentDescription = text },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Icon(
            painterResource(if (paused) R.drawable.ic_play else R.drawable.ic_pause),
            contentDescription = null,
            tint = LognaraPalette.Text,
            modifier = Modifier.size(18.dp),
        )
        Text(text, fontSize = 14.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text, maxLines = 1)
    }
}

@Composable
private fun liveStatusColor(status: LiveStatus, paused: Boolean): Color {
    val colors = LognaraSemanticColors
    return when {
        status == LiveStatus.CoreError || status == LiveStatus.Disconnected -> colors.statusErr
        status == LiveStatus.Reconnecting -> colors.statusWarn
        status == LiveStatus.Connecting -> colors.statusInfo
        paused -> colors.statusWarn
        else -> colors.statusOk
    }
}

private fun liveStatusTitle(status: LiveStatus, paused: Boolean): Int = when {
    status == LiveStatus.CoreError -> R.string.live_stream_status_core_error
    status == LiveStatus.Reconnecting -> R.string.live_stream_status_reconnecting
    status == LiveStatus.Disconnected -> R.string.live_stream_status_disconnected
    status == LiveStatus.Connecting -> R.string.live_stream_status_connecting
    paused -> R.string.live_stream_status_paused
    status == LiveStatus.Connected -> R.string.live_stream_status_connected
    else -> R.string.live_stream_status_disconnected
}

private fun noticeLabel(notice: LiveNotice): Int = when (notice) {
    LiveNotice.Lagged -> R.string.live_notice_lagged
    LiveNotice.CoreUnavailable -> R.string.live_status_core_error
    LiveNotice.InvalidData -> R.string.live_notice_invalid
}

private fun formatLiveTime(value: String): String = runCatching {
    DateTimeFormatter.ofPattern("HH:mm:ss", Locale.getDefault())
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrElse {
    value.substringAfter('T', value).take(8).takeIf { it.length == 8 } ?: value
}

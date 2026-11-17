package com.lognara.client.ui.screens.logs

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.RangeSelector
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.LognaraSemanticColors
import com.lognara.client.ui.theme.MachineTextStyle

@Composable
fun LogsScreen(
    api: LognaraApi,
    session: DeviceSession,
    onOpenEvent: (LogEvent) -> Unit,
) {
    val viewModel: LogsViewModel = viewModel(key = "logs-${session.deviceId}") {
        LogsViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    var query by rememberSaveable { mutableStateOf(state.query) }
    var service by rememberSaveable { mutableStateOf(state.service) }

    Column(Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 8.dp)) {
        RangeSelector(selected = state.range, onSelect = viewModel::setRange)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                label = { Text(stringResource(R.string.logs_search_label)) },
                singleLine = true,
                modifier = Modifier.weight(1f),
            )
            Button(onClick = { viewModel.search(query, service) }) { Text(stringResource(R.string.search)) }
        }
        OutlinedTextField(
            value = service,
            onValueChange = { service = it },
            label = { Text(stringResource(R.string.logs_service_label)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        LazyRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            items(LogLevel.entries.toList()) { level ->
                FilterChip(
                    selected = level in state.levels,
                    onClick = {
                        viewModel.setLevels(if (level in state.levels) state.levels - level else state.levels + level)
                    },
                    label = { Text(stringResource(levelLabel(level))) },
                )
            }
        }

        DataStateContent(
            state = state.result,
            isEmpty = { it.events.isEmpty() },
            onRetry = viewModel::reload,
            modifier = Modifier.weight(1f),
        ) { data ->
            Column(Modifier.fillMaxSize()) {
                data.summary?.let { summary ->
                    Text(
                        text = stringResource(R.string.logs_total, summary.total),
                        style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier.padding(start = 4.dp, top = 4.dp, bottom = 8.dp),
                    )
                    if (summary.buckets.isNotEmpty()) LevelHistogram(summary.buckets)
                }
                LazyColumn(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    items(data.events, key = { it.id }) { event ->
                        LogEventRow(event = event, onClick = { onOpenEvent(event) })
                    }
                    if (state.pagingError) item {
                        Text(stringResource(R.string.logs_page_error), color = MaterialTheme.colorScheme.error)
                        TextButton(onClick = viewModel::loadNextPage) { Text(stringResource(R.string.retry)) }
                    }
                    if (data.nextCursor != null) item {
                        TextButton(
                            onClick = viewModel::loadNextPage,
                            enabled = !state.loadingMore,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(stringResource(if (state.loadingMore) R.string.data_loading else R.string.logs_load_more))
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun LogEventRow(event: LogEvent, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val colors = LognaraSemanticColors
    Surface(
        modifier = modifier.fillMaxWidth().clickable(onClick = onClick),
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = MaterialTheme.shapes.medium,
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    stringResource(levelLabel(event.level)),
                    color = levelColor(event.level, colors),
                    style = MaterialTheme.typography.labelLarge,
                )
                Text(localTimestamp(event.timestamp), style = MachineTextStyle, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(event.service, style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Text(event.message, maxLines = 2, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
            Text(
                text = "${event.server} · ${event.backend}",
                style = MachineTextStyle,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun LevelHistogram(buckets: List<com.lognara.client.data.SummaryBucket>) {
    val colors = LognaraSemanticColors
    val levels = listOf(LogLevel.TRACE, LogLevel.DEBUG, LogLevel.INFO, LogLevel.WARN, LogLevel.ERROR, LogLevel.FATAL, LogLevel.UNKNOWN)
    val maxCount = buckets.maxOfOrNull { bucket ->
        bucket.levels.trace + bucket.levels.debug + bucket.levels.info + bucket.levels.warn + bucket.levels.error + bucket.levels.fatal + bucket.levels.unknown
    }?.coerceAtLeast(1) ?: 1
    Canvas(Modifier.fillMaxWidth().height(88.dp).padding(horizontal = 4.dp, vertical = 4.dp)) {
        if (buckets.isEmpty()) return@Canvas
        val slot = size.width / buckets.size
        val barWidth = (slot * 0.72f).coerceAtLeast(1f)
        buckets.forEachIndexed { index, bucket ->
            val counts = listOf(
                bucket.levels.trace,
                bucket.levels.debug,
                bucket.levels.info,
                bucket.levels.warn,
                bucket.levels.error,
                bucket.levels.fatal,
                bucket.levels.unknown,
            )
            var bottom = size.height
            counts.forEachIndexed { levelIndex, count ->
                if (count > 0) {
                    val height = (count.toFloat() / maxCount.toFloat()) * size.height
                    drawRect(
                        color = levelColor(levels[levelIndex], colors),
                        topLeft = Offset(index * slot + (slot - barWidth) / 2f, bottom - height),
                        size = Size(barWidth, height),
                    )
                    bottom -= height
                }
            }
        }
    }
}

internal fun levelLabel(level: LogLevel): Int = when (level) {
    LogLevel.TRACE -> R.string.log_level_trace
    LogLevel.DEBUG -> R.string.log_level_debug
    LogLevel.INFO -> R.string.log_level_info
    LogLevel.WARN -> R.string.log_level_warn
    LogLevel.ERROR -> R.string.log_level_error
    LogLevel.FATAL -> R.string.log_level_fatal
    LogLevel.UNKNOWN -> R.string.log_level_unknown
}

private fun levelColor(level: LogLevel, colors: com.lognara.client.ui.theme.LognaraColors): Color = when (level) {
    LogLevel.TRACE -> colors.trace
    LogLevel.DEBUG -> colors.debug
    LogLevel.INFO -> colors.info
    LogLevel.WARN -> colors.warn
    LogLevel.ERROR -> colors.error
    LogLevel.FATAL -> colors.fatal
    LogLevel.UNKNOWN -> colors.unknown
}

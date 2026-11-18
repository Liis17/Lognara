package com.lognara.client.ui.screens.logs

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.platform.LocalLocale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LevelCounts
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LogLevel
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.components.LogEventRow as SharedLogEventRow
import com.lognara.client.data.SummaryBucket
import com.lognara.client.data.SummaryResponse
import com.lognara.client.ui.components.ChipRow
import com.lognara.client.ui.components.EventCardListItem
import com.lognara.client.ui.components.FieldHint
import com.lognara.client.ui.components.FilterPill
import com.lognara.client.ui.components.LevelDot
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.ScreenHeader
import com.lognara.client.ui.components.SearchField
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.SettingsButton
import com.lognara.client.ui.components.levelColor
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.RangeSelector
import com.lognara.client.ui.screens.data.formatDataNumber
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.LognaraPalette
import java.net.URI
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

@Composable
fun LogsScreen(
    api: LognaraApi,
    session: DeviceSession,
    onOpenEvent: (LogEvent) -> Unit,
    onSettings: () -> Unit,
) {
    val viewModel: LogsViewModel = viewModel(key = "logs-${session.deviceId}") {
        LogsViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    var query by rememberSaveable { mutableStateOf(state.query) }
    var service by rememberSaveable { mutableStateOf(state.service) }
    var serviceDialog by rememberSaveable { mutableStateOf(false) }
    var serviceDraft by rememberSaveable { mutableStateOf("") }
    val host = remember(session.serverUrl) {
        runCatching { URI(session.serverUrl).host }.getOrNull() ?: session.serverUrl
    }

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = 16.dp),
        verticalArrangement = Arrangement.Top,
    ) {
        item {
            ScreenHeader(
                title = stringResource(R.string.tab_logs),
                subtitle = stringResource(R.string.logs_subtitle, host),
                actions = { SettingsButton(onSettings) },
            )
        }
        item(key = "space-search") { Spacer(Modifier.height(12.dp)) }
        item {
            SearchField(
                value = query,
                onValueChange = { query = it },
                placeholder = stringResource(R.string.logs_search_label),
                onSearch = { viewModel.search(query, service) },
                trailing = { FieldHint(stringResource(R.string.logs_service_hint)) },
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        item(key = "space-range") { Spacer(Modifier.height(12.dp)) }
        item {
            RangeSelector(
                selected = state.range,
                onSelect = viewModel::setRange,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        item(key = "space-filters") { Spacer(Modifier.height(12.dp)) }
        item {
            ChipRow(horizontalPadding = 16.dp) {
                FilterPill(
                    text = if (service.isBlank()) {
                        stringResource(R.string.logs_service_all)
                    } else {
                        stringResource(R.string.logs_service_selected, service)
                    },
                    onClick = {
                        serviceDraft = service
                        serviceDialog = true
                    },
                    trailing = {
                        Icon(
                            painterResource(R.drawable.ic_chevron_down),
                            contentDescription = null,
                            tint = LognaraPalette.TextMuted,
                            modifier = Modifier.size(16.dp),
                        )
                    },
                )
                LogLevel.entries.forEach { level ->
                    LevelFilterPill(
                        level = level,
                        selected = level in state.levels,
                        onClick = {
                            viewModel.setLevels(
                                if (level in state.levels) state.levels - level else state.levels + level,
                            )
                        },
                    )
                }
            }
        }
        item(key = "space-results") { Spacer(Modifier.height(12.dp)) }

        when (val result = state.result) {
            DataLoadState.Loading -> item {
                LogsStateCard(loading = true, text = stringResource(R.string.data_loading))
            }
            DataLoadState.Error -> item {
                LogsStateCard(
                    text = stringResource(R.string.data_load_error),
                    actionText = stringResource(R.string.retry),
                    onAction = viewModel::reload,
                )
            }
            is DataLoadState.Content -> {
                val data = result.value
                data.summary?.let { summary ->
                    item(key = "summary") {
                        SummaryCard(summary = summary, range = state.range)
                    }
                    item(key = "space-summary") { Spacer(Modifier.height(12.dp)) }
                }
                item(key = "event-day") {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 4.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        SectionLabel(dayLabel(data.events.firstOrNull()?.timestamp))
                        Text(
                            stringResource(R.string.logs_newest_first),
                            fontSize = 12.sp,
                            color = LognaraPalette.TextMuted,
                        )
                    }
                }
                item(key = "space-events") { Spacer(Modifier.height(12.dp)) }
                if (data.events.isEmpty()) {
                    item(key = "empty") {
                        Text(
                            stringResource(R.string.logs_empty),
                            fontSize = 14.sp,
                            color = LognaraPalette.TextMuted,
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 16.dp),
                        )
                    }
                    if (state.pagingError || data.nextCursor != null) {
                        item(key = "space-empty-page") { Spacer(Modifier.height(12.dp)) }
                    }
                } else {
                    itemsIndexed(data.events, key = { _, event -> event.id }) { index, event ->
                        EventCardListItem(
                            isFirst = index == 0,
                            isLast = index == data.events.lastIndex,
                        ) {
                            EventListRow(event = event, onClick = { onOpenEvent(event) })
                        }
                    }
                    if (state.pagingError || data.nextCursor != null) {
                        item(key = "space-event-page") { Spacer(Modifier.height(12.dp)) }
                    }
                }
                if (state.pagingError) item(key = "paging-error") {
                    Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(stringResource(R.string.logs_page_error), color = LognaraPalette.TextMuted)
                        TextButton(onClick = viewModel::loadNextPage) { Text(stringResource(R.string.retry)) }
                    }
                }
                if (state.pagingError && data.nextCursor != null) {
                    item(key = "space-next-page") { Spacer(Modifier.height(12.dp)) }
                }
                if (data.nextCursor != null) item(key = "next-page") {
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

    if (serviceDialog) {
        AlertDialog(
            onDismissRequest = { serviceDialog = false },
            title = { Text(stringResource(R.string.logs_service_dialog_title)) },
            text = {
                OutlinedTextField(
                    value = serviceDraft,
                    onValueChange = { serviceDraft = it },
                    label = { Text(stringResource(R.string.logs_service_dialog_hint)) },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    service = serviceDraft.trim()
                    viewModel.search(query, service)
                    serviceDialog = false
                }) {
                    Text(stringResource(R.string.logs_service_apply))
                }
            },
            dismissButton = {
                TextButton(onClick = {
                    service = ""
                    serviceDraft = ""
                    viewModel.search(query, service)
                    serviceDialog = false
                }) {
                    Text(stringResource(R.string.logs_service_all_action))
                }
            },
        )
    }
}

@Composable
private fun LevelFilterPill(level: LogLevel, selected: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .height(40.dp)
            .clip(RoundedCornerShape(999.dp))
            .background(if (selected) LognaraPalette.SurfaceRaised else Color.Transparent)
            .border(1.dp, LognaraPalette.Border.copy(alpha = 0.75f), RoundedCornerShape(999.dp))
            .clickable(role = Role.Checkbox, onClick = onClick)
            .semantics { this.selected = selected }
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        LevelDot(color = levelColor(level), filled = selected)
        Text(
            text = level.name,
            style = monoStyle(11, FontWeight.W600).copy(letterSpacing = 0.44.sp, lineHeight = 14.sp),
            color = if (selected) LognaraPalette.TextSecondary else LognaraPalette.TextMuted,
            maxLines = 1,
        )
    }
}

@Composable
private fun SummaryCard(summary: SummaryResponse, range: DataRange) {
    val locale = LocalLocale.current.platformLocale
    val chartDescription = stringResource(R.string.logs_chart_description, stringResource(range.label))
    val levels = listOf(
        LogLevel.TRACE,
        LogLevel.DEBUG,
        LogLevel.INFO,
        LogLevel.WARN,
        LogLevel.ERROR,
        LogLevel.FATAL,
        LogLevel.UNKNOWN,
    )
    LognaraCard {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.Bottom,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                SectionLabel(stringResource(R.string.logs_summary_title, stringResource(range.label)))
                Text(
                    formatDataNumber(summary.total, locale),
                    fontSize = 26.sp,
                    lineHeight = 33.sp,
                    fontWeight = FontWeight.W600,
                    color = LognaraPalette.Text,
                    maxLines = 1,
                )
            }
            Column(
                horizontalAlignment = Alignment.End,
                verticalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                levels.chunked(2).forEach { pair ->
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        pair.forEach { level -> SummaryLegendItem(level) }
                    }
                }
            }
        }
        SummaryChart(buckets = summary.buckets, levels = levels, contentDescription = chartDescription)
        val tickIndices = remember(summary.buckets.size) {
            if (summary.buckets.isEmpty()) emptyList() else listOf(
                0,
                summary.buckets.lastIndex / 3,
                (summary.buckets.lastIndex * 2) / 3,
                summary.buckets.lastIndex,
            ).distinct()
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            tickIndices.forEachIndexed { index, bucketIndex ->
                Text(
                    graphTimeLabel(summary.buckets[bucketIndex].timestamp, range, locale),
                    style = monoStyle(11),
                    color = LognaraPalette.TextMuted,
                    textAlign = when (index) {
                        0 -> TextAlign.Start
                        tickIndices.lastIndex -> TextAlign.End
                        else -> TextAlign.Center
                    },
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
private fun SummaryLegendItem(level: LogLevel) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        Box(
            Modifier
                .size(width = 12.dp, height = 2.dp)
                .clip(CircleShape)
                .background(levelColor(level)),
        )
        Text(
            level.name,
            style = monoStyle(10, FontWeight.W600).copy(letterSpacing = 0.3.sp, lineHeight = 14.sp),
            color = LognaraPalette.TextMuted,
            maxLines = 1,
        )
    }
}

@Composable
private fun SummaryChart(buckets: List<SummaryBucket>, levels: List<LogLevel>, contentDescription: String) {
    val seriesColors = levels.associateWith { level -> levelColor(level) }
    val seriesValues: Map<LogLevel, (LevelCounts) -> Long> = mapOf(
        LogLevel.TRACE to { it.trace },
        LogLevel.DEBUG to { it.debug },
        LogLevel.INFO to { it.info },
        LogLevel.WARN to { it.warn },
        LogLevel.ERROR to { it.error },
        LogLevel.FATAL to { it.fatal },
        LogLevel.UNKNOWN to { it.unknown },
    )
    val maxCount = buckets.maxOfOrNull { bucket ->
        seriesValues.values.maxOf { it(bucket.levels) }
    }?.coerceAtLeast(1L) ?: 1L

    Canvas(
        Modifier.fillMaxWidth().height(88.dp).semantics { this.contentDescription = contentDescription },
    ) {
        val inset = 4.dp.toPx()
        val graphHeight = (size.height - inset * 2).coerceAtLeast(1f)
        val gridColor = LognaraPalette.Divider
        listOf(0f, 1f / 3f, 2f / 3f, 1f).forEach { fraction ->
            val y = inset + graphHeight * fraction
            drawLine(gridColor, Offset(0f, y), Offset(size.width, y), strokeWidth = 1.dp.toPx())
        }
        if (buckets.isEmpty()) return@Canvas

        levels.forEach { level ->
            val getValue = seriesValues.getValue(level)
            if (buckets.none { getValue(it.levels) > 0L }) return@forEach
            val path = Path()
            buckets.forEachIndexed { index, bucket ->
                val x = if (buckets.size == 1) size.width / 2f else size.width * index / (buckets.lastIndex)
                val normalized = getValue(bucket.levels).toFloat() / maxCount.toFloat()
                val y = inset + graphHeight * (1f - normalized.coerceIn(0f, 1f))
                if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
            }
            drawPath(
                path = path,
                color = seriesColors.getValue(level),
                style = Stroke(width = 1.75.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round),
            )
        }
    }
}

@Composable
private fun LogsStateCard(
    text: String,
    loading: Boolean = false,
    actionText: String? = null,
    onAction: (() -> Unit)? = null,
) {
    LognaraCard(
        modifier = Modifier.padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (loading) CircularProgressIndicator(
                modifier = Modifier.size(22.dp),
                color = LognaraPalette.Accent,
                strokeWidth = 2.dp,
            )
            Text(text, color = LognaraPalette.TextMuted, modifier = Modifier.weight(1f))
        }
        if (actionText != null && onAction != null) {
            TextButton(onClick = onAction) { Text(actionText) }
        }
    }
}

@Composable
private fun EventListRow(event: LogEvent, onClick: () -> Unit) {
    SharedLogEventRow(
        level = event.level,
        time = eventTime(event.timestamp),
        service = event.service,
        server = event.server,
        message = event.message,
        onClick = onClick,
    )
}

private fun eventTime(value: String): String {
    val formatted = localTimestamp(value)
    return formatted.substringAfter(' ', formatted)
}

@Composable
private fun dayLabel(value: String?): String {
    val date = value?.let { runCatching { Instant.parse(it).atZone(ZoneId.systemDefault()).toLocalDate() }.getOrNull() }
        ?: LocalDate.now()
    val formatted = date.format(DateTimeFormatter.ofPattern("d MMMM", Locale.forLanguageTag("ru")))
    return if (date == LocalDate.now()) stringResource(R.string.logs_today, formatted) else formatted
}

private fun graphTimeLabel(value: String, range: DataRange, locale: Locale): String = runCatching {
    val pattern = if (range == DataRange.SevenDays) "dd.MM" else "HH:mm"
    DateTimeFormatter.ofPattern(pattern, locale)
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrDefault(value)

package com.lognara.client.ui.screens.metrics

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.MetricKey
import com.lognara.client.data.MetricSource
import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.SeriesReply
import com.lognara.client.ui.components.ChipRow
import com.lognara.client.ui.components.DetailTopBar
import com.lognara.client.ui.components.FilterPill
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.RoundIconButton
import com.lognara.client.ui.components.ScreenHeader
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.SettingsButton
import com.lognara.client.ui.components.StatTile
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.RangeSelector
import com.lognara.client.ui.screens.data.formatDataNumber
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import java.util.concurrent.CancellationException
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.log10
import kotlin.math.pow
import kotlin.math.roundToInt

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun MetricsScreen(
    api: LognaraApi,
    session: DeviceSession,
    onOpenMetric: (SeriesMetric) -> Unit,
    onSettings: () -> Unit,
) {
    val viewModel: MetricsViewModel = viewModel(key = "metrics-${session.deviceId}") {
        MetricsViewModel(api, session)
    }
    val overview by viewModel.overview.collectAsStateWithLifecycle()
    val selectedSource by viewModel.selectedSource.collectAsStateWithLifecycle()
    val catalog by viewModel.catalog.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val favoriteCount = (overview as? DataLoadState.Content<MetricsOverview>)?.value?.favorites?.size ?: 0

    Column(Modifier.fillMaxSize()) {
        ScreenHeader(
            title = stringResource(R.string.tab_metrics),
            subtitle = stringResource(R.string.metrics_favorites_subtitle, favoriteCount),
            actions = { SettingsButton(onSettings) },
        )
        PullToRefreshBox(
            isRefreshing = overview is DataLoadState.Loading,
            onRefresh = viewModel::reload,
            modifier = Modifier.weight(1f),
        ) {
            DataStateContent(
                state = overview,
                isEmpty = { false },
                onRetry = viewModel::reload,
                modifier = Modifier.fillMaxSize(),
            ) { data ->
                LazyColumn(
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    item {
                        RangeSelector(
                            selected = state.miniRange,
                            onSelect = viewModel::setMiniRange,
                            ranges = metricRanges,
                        )
                    }
                    if (data.favorites.isEmpty()) {
                        item {
                            Text(
                                stringResource(R.string.metric_favorites_empty),
                                color = LognaraPalette.TextMuted,
                                modifier = Modifier.padding(horizontal = 4.dp),
                            )
                        }
                    } else {
                        items(data.favorites, key = { metricIdentity(it.key) }) { favorite ->
                            val metric = favorite.metric
                            if (metric == null) {
                                MissingFavoriteCard(favorite.key)
                            } else {
                                FavoriteMetricCard(
                                    metric = metric,
                                    range = state.miniRange,
                                    viewModel = viewModel,
                                    onClick = { onOpenMetric(metric) },
                                )
                            }
                        }
                    }

                    item { SectionLabel(stringResource(R.string.metric_sources), Modifier.padding(start = 4.dp)) }
                    if (selectedSource == null) {
                        if (data.sources.isEmpty()) {
                            item {
                                Text(
                                    stringResource(R.string.metric_sources_empty),
                                    color = LognaraPalette.TextMuted,
                                    modifier = Modifier.padding(horizontal = 4.dp),
                                )
                            }
                        } else {
                            items(data.sources, key = { "${it.server}\u0000${it.service}" }) { source ->
                                SourceCard(source, onClick = { viewModel.selectSource(source) })
                            }
                        }
                    } else {
                        item {
                            Row(
                                Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.SpaceBetween,
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    stringResource(
                                        R.string.metric_source_title,
                                        selectedSource?.server.orEmpty(),
                                        selectedSource?.service.orEmpty(),
                                    ),
                                    color = LognaraPalette.Text,
                                    fontWeight = FontWeight.W600,
                                    modifier = Modifier.weight(1f),
                                )
                                FilterPill(
                                    text = stringResource(R.string.metric_all_sources),
                                    onClick = { viewModel.selectSource(null) },
                                )
                            }
                        }
                        when (val result = catalog) {
                            DataLoadState.Loading -> item { LoadingMetricCatalog() }
                            DataLoadState.Error -> item {
                                Column {
                                    Text(stringResource(R.string.metric_catalog_error), color = LognaraSemanticColors.error)
                                    TextButton(onClick = viewModel::reloadCatalog) { Text(stringResource(R.string.retry)) }
                                }
                            }
                            is DataLoadState.Content -> {
                                val favorites = data.favorites.mapTo(HashSet()) { metricIdentity(it.key) }
                                val metrics = result.value.filterNot { metricIdentity(it) in favorites }
                                if (metrics.isEmpty()) {
                                    item {
                                        Text(
                                            stringResource(R.string.metric_catalog_empty),
                                            color = LognaraPalette.TextMuted,
                                            modifier = Modifier.padding(horizontal = 4.dp),
                                        )
                                    }
                                } else {
                                    items(metrics, key = { metricIdentity(it) }) { metric ->
                                        MetricCatalogCard(metric, onClick = { onOpenMetric(metric) })
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

private val metricRanges = listOf(DataRange.OneHour, DataRange.TwentyFourHours, DataRange.SevenDays)

@Composable
private fun SourceCard(source: MetricSource, onClick: () -> Unit) {
    LognaraCard(
        onClick = onClick,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        Text(
            source.server.ifBlank { stringResource(R.string.metric_server_unnamed) },
            fontSize = 16.sp,
            fontWeight = FontWeight.W600,
            color = LognaraPalette.Text,
        )
        Text(source.service, style = monoStyle(12), color = LognaraPalette.TextMuted)
    }
}

@Composable
private fun MetricCatalogCard(metric: SeriesMetric, onClick: () -> Unit) {
    LognaraCard(
        onClick = onClick,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name,
            fontSize = 16.sp,
            fontWeight = FontWeight.W600,
            color = LognaraPalette.Text,
        )
        Text(stringResource(R.string.metric_identity, metric.server, metric.service, metric.name), style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text(
            stringResource(R.string.metric_catalog_metadata, originLabel(metric.origin), metric.definition?.unit.orEmpty()),
            fontSize = 12.sp,
            color = LognaraPalette.TextMuted,
        )
        metric.definition?.description?.takeIf(String::isNotBlank)?.let {
            Text(it, fontSize = 13.sp, color = LognaraPalette.TextSecondary)
        }
    }
}

@Composable
private fun MissingFavoriteCard(key: MetricKey) {
    LognaraCard(
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(key.name, fontSize = 16.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
        Text(stringResource(R.string.metric_favorite_missing), fontSize = 12.sp, color = LognaraPalette.TextMuted)
        Text(stringResource(R.string.metric_identity, key.server, key.service, key.name), style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun FavoriteMetricCard(
    metric: SeriesMetric,
    range: DataRange,
    viewModel: MetricsViewModel,
    onClick: () -> Unit,
) {
    val key = metricIdentity(metric)
    val lifecycleOwner = LocalLifecycleOwner.current
    var isResumed by remember(lifecycleOwner) {
        mutableStateOf(lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))
    }
    var retry by rememberSaveable(key) { mutableIntStateOf(0) }
    var preview by remember(key, range) { mutableStateOf<DataLoadState<SeriesReply>>(DataLoadState.Loading) }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, _ ->
            isResumed = lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(metric, range, retry, isResumed) {
        if (!isResumed || metric.definition?.enabled == false) return@LaunchedEffect
        preview = DataLoadState.Loading
        try {
            preview = DataLoadState.Content(viewModel.loadPreview(metric, range))
        } catch (exception: CancellationException) {
            throw exception
        } catch (_: ApiException.Unauthorized) {
            return@LaunchedEffect
        } catch (_: Exception) {
            preview = DataLoadState.Error
        }
    }

    val chart = (preview as? DataLoadState.Content<SeriesReply>)?.value
    val values = chart?.series?.flatMap { it.values }?.filterNotNull()?.filter(Double::isFinite).orEmpty()
    val latest = chart?.series?.firstOrNull()?.values?.lastOrNull { it != null && it.isFinite() }
    val color = metricColor(metric)
    val unit = chart?.unit?.takeIf(String::isNotBlank)
        ?: metric.definition?.unit?.takeIf(String::isNotBlank)
        ?: stringResource(R.string.metrics_default_unit)

    LognaraCard(
        onClick = onClick,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.Top,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name,
                    fontSize = 16.sp,
                    lineHeight = 21.sp,
                    fontWeight = FontWeight.W600,
                    color = LognaraPalette.Text,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    stringResource(R.string.metric_identity, metric.server, metric.service, metric.name),
                    style = monoStyle(12),
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Column(horizontalAlignment = Alignment.End) {
                Text(
                    latest?.let { number(it) } ?: "—",
                    fontSize = 26.sp,
                    lineHeight = 33.sp,
                    fontWeight = FontWeight.W600,
                    color = LognaraPalette.Text,
                    maxLines = 1,
                )
                Text(
                    stringResource(R.string.metrics_unit_per_step, unit, intervalLabel(range)),
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                )
            }
        }

        when {
            metric.definition?.enabled == false -> Text(
                stringResource(R.string.metric_definition_disabled),
                fontSize = 12.sp,
                color = LognaraPalette.TextMuted,
            )
            chart != null && chartScale(chart.series.flatMap { it.values }) != null -> FavoriteSparkline(chart, color)
            chart != null -> Text(stringResource(R.string.metric_chart_empty), fontSize = 12.sp, color = LognaraPalette.TextMuted)
            preview is DataLoadState.Error -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.metric_preview_error), fontSize = 12.sp, color = LognaraSemanticColors.error, modifier = Modifier.weight(1f))
                TextButton(onClick = { retry++ }) { Text(stringResource(R.string.retry)) }
            }
            preview is DataLoadState.Loading -> Text(stringResource(R.string.metric_preview_loading), fontSize = 12.sp, color = LognaraPalette.TextMuted)
        }

        if (values.isNotEmpty()) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.metric_origin, originLabel(metric.origin)),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                )
                Text(
                    stringResource(R.string.metrics_min_max, number(values.minOrNull()!!), number(values.maxOrNull()!!)),
                    style = monoStyle(12),
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                    textAlign = TextAlign.End,
                )
            }
        }
    }
}

@Composable
private fun FavoriteSparkline(chart: SeriesReply, color: Color) {
    val scale = chartScale(chart.series.flatMap { it.values }) ?: return
    val values = chart.series.firstOrNull()?.values.orEmpty()
    val latestIndex = values.indexOfLast { it != null && it.isFinite() }
    Canvas(Modifier.fillMaxWidth().height(56.dp)) {
        val plotWidth = size.width
        val plotHeight = size.height - 6.dp.toPx()
        val path = Path()
        var previous = false
        val denominator = (values.size - 1).coerceAtLeast(1)
        values.forEachIndexed { index, value ->
            if (value == null || !value.isFinite()) {
                previous = false
            } else {
                val x = plotWidth * index / denominator
                val ratio = ((value - scale.minimum) / (scale.maximum - scale.minimum)).toFloat().coerceIn(0f, 1f)
                val y = 3.dp.toPx() + plotHeight * (1f - ratio)
                if (previous) path.lineTo(x, y) else path.moveTo(x, y)
                previous = true
            }
        }
        drawPath(path, color, style = Stroke(width = 1.75.dp.toPx()))
        values.getOrNull(latestIndex)?.takeIf(Double::isFinite)?.let { value ->
            val x = plotWidth * latestIndex / denominator
            val ratio = ((value - scale.minimum) / (scale.maximum - scale.minimum)).toFloat().coerceIn(0f, 1f)
            val y = 3.dp.toPx() + plotHeight * (1f - ratio)
            drawCircle(color, radius = 3.dp.toPx(), center = Offset(x, y))
        }
    }
}

@Composable
private fun LoadingMetricCatalog() {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
        Text(stringResource(R.string.metric_catalog_loading), color = LognaraPalette.TextMuted)
    }
}

@Composable
fun MetricDetailsScreen(
    api: LognaraApi,
    session: DeviceSession,
    key: MetricKey,
    onBack: () -> Unit,
) {
    val viewModel: MetricDetailsViewModel = viewModel(key = "metric-${session.deviceId}-${metricIdentity(key)}") {
        MetricDetailsViewModel(api, session, key)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    var environmentMenu by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize()) {
        DetailTopBar(
            title = stringResource(R.string.metric_detail_title),
            onBack = onBack,
            actions = {
                RoundIconButton(
                    icon = R.drawable.ic_refresh,
                    contentDescription = stringResource(R.string.metrics_refresh_description),
                    onClick = viewModel::retryMetric,
                )
            },
        )
        DataStateContent(
            state = state.metric,
            isEmpty = { it == null },
            onRetry = viewModel::retryMetric,
            emptyText = stringResource(R.string.metric_missing),
            modifier = Modifier.weight(1f).padding(horizontal = 16.dp),
        ) { metric ->
            if (metric != null) {
                LazyColumn(
                    contentPadding = PaddingValues(bottom = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    item {
                        Column(
                            Modifier.padding(horizontal = 4.dp),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            Text(
                                metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name,
                                fontSize = 26.sp,
                                lineHeight = 34.sp,
                                fontWeight = FontWeight.W600,
                                color = LognaraPalette.Text,
                                modifier = Modifier.semantics { heading() },
                            )
                            Text(
                                stringResource(R.string.metric_identity, metric.server, metric.service, metric.name),
                                style = monoStyle(12),
                                color = LognaraPalette.TextMuted,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            metric.definition?.description?.takeIf(String::isNotBlank)?.let {
                                Text(it, fontSize = 13.sp, lineHeight = 20.sp, color = LognaraPalette.TextSecondary)
                            }
                            if (metric.definition?.enabled == false) {
                                Text(stringResource(R.string.metric_definition_disabled), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                            }
                        }
                    }
                    item {
                        RangeSelector(state.range, viewModel::setRange, ranges = metricRanges)
                    }
                    item {
                        MetricSplitSelector(state.split, viewModel::setSplit)
                    }
                    if (metric.environments.isNotEmpty()) {
                        item {
                            Box {
                                FilterPill(
                                    text = stringResource(
                                        R.string.metrics_environment_filter,
                                        state.environment.ifBlank { stringResource(R.string.metric_environment_all) },
                                    ),
                                    onClick = { environmentMenu = true },
                                )
                                DropdownMenu(expanded = environmentMenu, onDismissRequest = { environmentMenu = false }) {
                                    DropdownMenuItem(
                                        text = { Text(stringResource(R.string.metric_environment_all)) },
                                        onClick = { viewModel.setEnvironment(""); environmentMenu = false },
                                    )
                                    metric.environments.forEach { environment ->
                                        DropdownMenuItem(
                                            text = { Text(environment) },
                                            onClick = { viewModel.setEnvironment(environment); environmentMenu = false },
                                        )
                                    }
                                }
                            }
                        }
                    }
                    when (val chart = state.chart) {
                        DataLoadState.Loading -> item {
                            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                                Text(stringResource(R.string.metric_chart_loading), color = LognaraPalette.TextMuted)
                            }
                        }
                        DataLoadState.Error -> item {
                            Column {
                                Text(stringResource(R.string.metric_chart_error), color = LognaraSemanticColors.error)
                                TextButton(onClick = viewModel::retryChart) { Text(stringResource(R.string.retry)) }
                            }
                        }
                        is DataLoadState.Content -> {
                            val values = chart.value.series.flatMap { it.values }.filterNotNull().filter(Double::isFinite)
                            if (values.isEmpty()) {
                                item { Text(stringResource(R.string.metric_chart_empty), color = LognaraPalette.TextMuted) }
                            } else {
                                item {
                                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                                        StatTile(
                                            label = stringResource(R.string.metrics_maximum),
                                            value = number(values.maxOrNull()!!),
                                            modifier = Modifier.weight(1f),
                                        )
                                        StatTile(
                                            label = stringResource(R.string.metrics_minimum),
                                            value = number(values.minOrNull()!!),
                                            modifier = Modifier.weight(1f),
                                        )
                                    }
                                }
                                item { MetricSeriesChart(chart.value, metric, state.range) }
                                item {
                                    Text(
                                        stringResource(
                                            R.string.metrics_details_footer,
                                            metricKind(chart.value.kind).replaceFirstChar { it.uppercaseChar() },
                                            aggregationLabel(chart.value.aggregation),
                                            originLabel(metric.origin),
                                        ),
                                        fontSize = 12.sp,
                                        lineHeight = 18.sp,
                                        color = LognaraPalette.TextMuted,
                                        modifier = Modifier.padding(horizontal = 4.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun MetricSplitSelector(selected: String, onSelect: (String) -> Unit) {
    val options = listOf(
        "none" to R.string.metric_split_sum,
        "instance" to R.string.metric_split_instance,
        "labels" to R.string.metric_split_labels,
    )
    ChipRow(horizontalPadding = 0.dp, modifier = Modifier.selectableGroup()) {
        options.forEach { (value, label) ->
            MetricSplitChip(
                text = stringResource(label),
                selected = selected == value,
                onClick = { onSelect(value) },
            )
        }
    }
}

@Composable
private fun MetricSplitChip(text: String, selected: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(999.dp)
    Row(
        modifier = Modifier
            .height(40.dp)
            .clip(shape)
            .background(if (selected) LognaraPalette.AccentSoft else LognaraPalette.Surface)
            .border(1.dp, if (selected) LognaraPalette.Accent.copy(alpha = 0.4f) else LognaraPalette.BorderStrong, shape)
            .selectable(selected = selected, role = Role.Tab, onClick = onClick)
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(text, fontSize = 13.sp, fontWeight = FontWeight.W500, color = LognaraPalette.TextSecondary, maxLines = 1)
    }
}

@Composable
private fun MetricSeriesChart(chart: SeriesReply, metric: SeriesMetric, range: DataRange) {
    val scale = chartScale(chart.series.flatMap { it.values }) ?: return
    val bucketCount = maxOf(chart.buckets.size, chart.series.maxOfOrNull { it.values.size } ?: 0)
    if (bucketCount == 0) return
    val initialIndex = remember(chart) { maximumPointIndex(chart)?.let { nearestPointIndex(chart, it) } ?: 0 }
    var selectedIndex by remember(chart) { mutableIntStateOf(initialIndex) }
    val seriesColors = chart.series.indices.map { MetricSeriesColors[it % MetricSeriesColors.size] }
    val selectedPoint = chart.series.mapIndexedNotNull { seriesIndex, line ->
        line.values.getOrNull(selectedIndex)?.takeIf(Double::isFinite)?.let { Triple(seriesIndex, line, it) }
    }.firstOrNull()
    val selectedTimestamp = chart.buckets.getOrNull(selectedIndex)?.let(::metricTimestamp).orEmpty()
    val axis = remember(scale) { chartAxis(scale) }
    val stepText = intervalLabel(range)
    val chartDescription = stringResource(
        R.string.metrics_chart_accessibility,
        metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name,
        stringResource(range.label),
        number(scale.maximum),
        number(scale.minimum),
    )
    val accessibleActionLabel = stringResource(R.string.metrics_chart_accessible_action)
    val selectIndex: (Float, Float) -> Unit = { x, plotWidth ->
        val target = ((x / plotWidth.coerceAtLeast(1f)) * (bucketCount - 1)).roundToInt().coerceIn(0, bucketCount - 1)
        nearestPointIndex(chart, target)?.let { selectedIndex = it }
    }

    LognaraCard(
        contentPadding = PaddingValues(16.dp),
        shape = RoundedCornerShape(20.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(
                Modifier.weight(1f).horizontalScroll(rememberScrollState()),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                chart.series.forEachIndexed { index, line ->
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Canvas(Modifier.width(12.dp).height(2.dp)) {
                            drawLine(seriesColors[index], Offset(0f, size.height / 2), Offset(size.width, size.height / 2), 2.dp.toPx())
                        }
                        Text(
                            line.label.ifBlank { stringResource(R.string.metric_series_total) },
                            style = monoStyle(11, FontWeight.W600),
                            color = LognaraPalette.TextMuted,
                            maxLines = 1,
                        )
                    }
                }
            }
            Text(
                stringResource(R.string.metrics_chart_step, stepText),
                fontSize = 12.sp,
                color = LognaraPalette.TextMuted,
                maxLines = 1,
            )
        }

        BoxWithConstraints(Modifier.fillMaxWidth().height(190.dp)) {
            val axisWidth = 52.dp
            val graphWidth = maxWidth - axisWidth
            val graphWidthPx = with(LocalDensity.current) { graphWidth.toPx() }
            Canvas(
                Modifier
                    .fillMaxSize()
                    .pointerInput(bucketCount, graphWidthPx) {
                        detectTapGestures { selectIndex(it.x, graphWidthPx) }
                    }
                    .pointerInput(bucketCount, graphWidthPx) {
                        detectHorizontalDragGestures(
                            onDragStart = { selectIndex(it.x, graphWidthPx) },
                            onHorizontalDrag = { change, _ ->
                                change.consume()
                                selectIndex(change.position.x, graphWidthPx)
                            },
                        )
                    }
                    .semantics {
                        contentDescription = chartDescription
                        onClick(label = accessibleActionLabel) {
                            selectIndex(graphWidthPx / 2f, graphWidthPx)
                            true
                        }
                    },
            ) {
                val axisWidthPx = axisWidth.toPx()
                val plotWidth = (size.width - axisWidthPx).coerceAtLeast(1f)
                val verticalPadding = 6.dp.toPx()
                val plotHeight = (size.height - verticalPadding * 2).coerceAtLeast(1f)
                axis.ticks.forEach { tick ->
                    val ratio = ((tick - axis.minimum) / (axis.maximum - axis.minimum)).toFloat()
                    val y = verticalPadding + plotHeight * (1f - ratio)
                    drawLine(
                        LognaraPalette.Divider,
                        Offset(0f, y),
                        Offset(plotWidth, y),
                        1.dp.toPx(),
                    )
                }
                chart.series.forEachIndexed { seriesIndex, line ->
                    val path = Path()
                    var previous = false
                    val denominator = (bucketCount - 1).coerceAtLeast(1)
                    line.values.forEachIndexed { index, value ->
                        if (index >= bucketCount || value == null || !value.isFinite()) {
                            previous = false
                        } else {
                            val x = plotWidth * index / denominator
                            val ratio = ((value - axis.minimum) / (axis.maximum - axis.minimum)).toFloat().coerceIn(0f, 1f)
                            val y = verticalPadding + plotHeight * (1f - ratio)
                            if (previous) path.lineTo(x, y) else path.moveTo(x, y)
                            previous = true
                        }
                    }
                    drawPath(path, seriesColors[seriesIndex], style = Stroke(width = 2.dp.toPx()))
                }

                val selectedX = plotWidth * selectedIndex / (bucketCount - 1).coerceAtLeast(1)
                drawLine(
                    color = LognaraPalette.TextMuted.copy(alpha = 0.75f),
                    start = Offset(selectedX, 0f),
                    end = Offset(selectedX, size.height),
                    strokeWidth = 1.dp.toPx(),
                    pathEffect = PathEffect.dashPathEffect(floatArrayOf(3.dp.toPx(), 3.dp.toPx())),
                )
                selectedPoint?.let { (seriesIndex, _, value) ->
                    val ratio = ((value - axis.minimum) / (axis.maximum - axis.minimum)).toFloat().coerceIn(0f, 1f)
                    val center = Offset(selectedX, verticalPadding + plotHeight * (1f - ratio))
                    drawCircle(LognaraPalette.Background, radius = 5.dp.toPx(), center = center)
                    drawCircle(seriesColors[seriesIndex], radius = 4.dp.toPx(), center = center, style = Stroke(width = 2.dp.toPx()))
                }
            }

            Column(
                modifier = Modifier.align(Alignment.TopEnd).width(axisWidth).height(172.dp).padding(start = 3.dp),
                verticalArrangement = Arrangement.SpaceBetween,
                horizontalAlignment = Alignment.End,
            ) {
                axis.ticks.forEach { value ->
                    Text(number(value), style = monoStyle(10), color = LognaraPalette.TextMuted, maxLines = 1)
                }
            }

            if (selectedPoint != null && selectedTimestamp.isNotBlank()) {
                val tooltipOffset = (graphWidth * selectedIndex / (bucketCount - 1).coerceAtLeast(1) - 110.dp)
                    .coerceIn(0.dp, (maxWidth - 220.dp).coerceAtLeast(0.dp))
                Row(
                    modifier = Modifier
                        .align(Alignment.TopStart)
                        .offset(x = tooltipOffset, y = 4.dp)
                        .widthIn(max = 220.dp)
                        .clip(RoundedCornerShape(8.dp))
                        .background(LognaraPalette.Surface.copy(alpha = 0.96f))
                        .border(1.dp, LognaraPalette.BorderStrong, RoundedCornerShape(8.dp))
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(selectedTimestamp, style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1)
                    Text(number(selectedPoint.third), style = monoStyle(12, FontWeight.W500), color = LognaraPalette.Text, maxLines = 1)
                }
            }
        }

        if (chart.buckets.isNotEmpty()) {
            val bucketTimes = listOf(chart.buckets.first(), chart.buckets[chart.buckets.lastIndex / 2], chart.buckets.last())
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                bucketTimes.forEachIndexed { index, bucket ->
                    Text(
                        metricTimestamp(bucket),
                        style = monoStyle(11),
                        color = LognaraPalette.TextMuted,
                        maxLines = 1,
                        textAlign = when (index) {
                            0 -> TextAlign.Start
                            1 -> TextAlign.Center
                            else -> TextAlign.End
                        },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

internal fun maximumPointIndex(chart: SeriesReply): Int? {
    var maximum: Double? = null
    var maximumIndex: Int? = null
    chart.series.forEach { line ->
        line.values.forEachIndexed { index, value ->
            if (value != null && value.isFinite() && (maximum == null || value > maximum!!)) {
                maximum = value
                maximumIndex = index
            }
        }
    }
    return maximumIndex
}

internal fun nearestPointIndex(chart: SeriesReply, target: Int): Int? {
    val count = minOf(chart.buckets.size, chart.series.maxOfOrNull { it.values.size } ?: 0)
    return (0 until count)
        .filter { index -> chart.series.any { it.values.getOrNull(index)?.isFinite() == true } }
        .minByOrNull { abs(it - target) }
}

internal data class ChartAxis(val minimum: Double, val maximum: Double, val ticks: List<Double>)

internal fun chartAxis(scale: ChartScale): ChartAxis {
    val rawStep = (scale.maximum - scale.minimum) / 3
    val magnitude = 10.0.pow(floor(log10(rawStep)))
    val normalized = rawStep / magnitude
    val factor = when {
        normalized <= 1 -> 1.0
        normalized <= 2 -> 2.0
        normalized <= 2.5 -> 2.5
        normalized <= 4.5 -> 4.0
        normalized <= 7 -> 5.0
        else -> 10.0
    }
    val step = factor * magnitude
    val roundingUnit = step / 4
    val minimum = floor(scale.minimum / roundingUnit) * roundingUnit
    val maximum = ceil(scale.maximum / step) * step
    return ChartAxis(minimum, maximum, (0..3).map { maximum - step * it })
}

private val MetricSeriesColors = listOf(
    LognaraPalette.Accent,
    LognaraPalette.TextSecondary,
    Color(0xFF9AA6E8),
    Color(0xFFF87171),
)

private fun metricTimestamp(value: String): String = runCatching {
    DateTimeFormatter.ofPattern("dd.MM HH:mm", Locale.getDefault())
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrDefault(value)

@Composable
private fun intervalLabel(range: DataRange): String {
    val seconds = seriesStepSeconds(range)
    return when {
        seconds % 3_600 == 0L -> stringResource(R.string.metrics_step_hours, seconds / 3_600)
        seconds % 60 == 0L -> stringResource(R.string.metrics_step_minutes, seconds / 60)
        else -> stringResource(R.string.metrics_step_seconds, seconds)
    }
}

@Composable
private fun metricColor(metric: SeriesMetric): Color {
    val identity = "${metric.definition?.title.orEmpty()} ${metric.name}".lowercase(Locale.ROOT)
    return if (listOf("error", "ошиб", "4xx", "5xx").any(identity::contains)) {
        LognaraSemanticColors.error
    } else {
        LognaraPalette.Accent
    }
}

@Composable
private fun originLabel(origin: String): String = stringResource(
    when (origin) {
        "gauge" -> R.string.metric_origin_gauge
        "rule" -> R.string.metric_origin_rule
        else -> R.string.metric_origin_counter
    },
)

@Composable
private fun metricKind(kind: String): String = stringResource(
    when (kind) {
        "gauge" -> R.string.metric_kind_gauge
        "cumulative" -> R.string.metric_kind_cumulative
        else -> R.string.metric_kind_delta
    },
)

@Composable
private fun aggregationLabel(value: String): String = stringResource(
    when (value) {
        "avg" -> R.string.metric_aggregation_avg
        "min" -> R.string.metric_aggregation_min
        "max" -> R.string.metric_aggregation_max
        "last" -> R.string.metric_aggregation_last
        else -> R.string.metric_aggregation_sum
    },
)

private fun number(value: Double): String = formatDataNumber(value, Locale.forLanguageTag("ru"))

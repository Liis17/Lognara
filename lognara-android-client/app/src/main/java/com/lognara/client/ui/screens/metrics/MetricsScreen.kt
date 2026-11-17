package com.lognara.client.ui.screens.metrics

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.MetricKey
import com.lognara.client.data.MetricSource
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.SeriesReply
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.MachineTextStyle
import java.text.NumberFormat
import java.util.concurrent.CancellationException

@Composable
fun MetricsScreen(
    api: LognaraApi,
    session: DeviceSession,
    onOpenMetric: (SeriesMetric) -> Unit,
) {
    val viewModel: MetricsViewModel = viewModel(key = "metrics-${session.deviceId}") {
        MetricsViewModel(api, session)
    }
    val overview by viewModel.overview.collectAsStateWithLifecycle()
    val selectedSource by viewModel.selectedSource.collectAsStateWithLifecycle()
    val catalog by viewModel.catalog.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()

    DataStateContent(
        state = overview,
        isEmpty = { false },
        onRetry = viewModel::reload,
        modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp),
    ) { data ->
        LazyColumn(
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
        ) {
            item {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(onClick = viewModel::reload) { Text(stringResource(R.string.metric_refresh)) }
                }
            }
            item { Text(stringResource(R.string.metric_favorites), style = MaterialTheme.typography.titleLarge) }
            item {
                MetricRangeSelector(state.miniRange, viewModel::setMiniRange)
            }
            if (data.favorites.isEmpty()) {
                item { Text(stringResource(R.string.metric_favorites_empty), color = MaterialTheme.colorScheme.onSurfaceVariant) }
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
            item { Text(stringResource(R.string.metric_sources), style = MaterialTheme.typography.titleLarge) }
            if (selectedSource == null) {
                if (data.sources.isEmpty()) {
                    item { Text(stringResource(R.string.metric_sources_empty), color = MaterialTheme.colorScheme.onSurfaceVariant) }
                } else {
                    items(data.sources, key = { "${it.server}\u0000${it.service}" }) { source ->
                        SourceCard(source, onClick = { viewModel.selectSource(source) })
                    }
                }
            } else {
                item {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(
                            stringResource(R.string.metric_source_title, selectedSource?.server.orEmpty(), selectedSource?.service.orEmpty()),
                            style = MaterialTheme.typography.titleMedium,
                        )
                        TextButton(onClick = { viewModel.selectSource(null) }) {
                            Text(stringResource(R.string.metric_all_sources))
                        }
                    }
                }
                when (val result = catalog) {
                    DataLoadState.Loading -> item { LoadingMetricCatalog() }
                    DataLoadState.Error -> item {
                        Column {
                            Text(stringResource(R.string.metric_catalog_error), color = MaterialTheme.colorScheme.error)
                            TextButton(onClick = viewModel::reloadCatalog) { Text(stringResource(R.string.retry)) }
                        }
                    }
                    is DataLoadState.Content -> {
                        val favorites = data.favorites.mapTo(HashSet()) { metricIdentity(it.key) }
                        val metrics = result.value.filterNot { metricIdentity(it) in favorites }
                        if (metrics.isEmpty()) {
                            item { Text(stringResource(R.string.metric_catalog_empty), color = MaterialTheme.colorScheme.onSurfaceVariant) }
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

@Composable
private fun MetricRangeSelector(selected: DataRange, onSelect: (DataRange) -> Unit) {
    val ranges = listOf(DataRange.OneHour, DataRange.TwentyFourHours, DataRange.SevenDays)
    LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
        items(ranges, key = { it.name }) { range ->
            FilterChip(
                selected = selected == range,
                onClick = { onSelect(range) },
                label = { Text(stringResource(range.label)) },
            )
        }
    }
}

@Composable
private fun SourceCard(source: MetricSource, onClick: () -> Unit) {
    Card(Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(source.server.ifBlank { stringResource(R.string.metric_server_unnamed) }, style = MaterialTheme.typography.titleMedium)
            Text(source.service, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun MetricCatalogCard(metric: SeriesMetric, onClick: () -> Unit) {
    Card(Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name, style = MaterialTheme.typography.titleMedium)
            Text(metric.name, style = MachineTextStyle)
            Text(
                stringResource(R.string.metric_catalog_metadata, originLabel(metric.origin), metric.definition?.unit.orEmpty()),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            metric.definition?.description?.takeIf(String::isNotBlank)?.let { Text(it) }
        }
    }
}

@Composable
private fun MissingFavoriteCard(key: MetricKey) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(key.name, style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.metric_favorite_missing), color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(stringResource(R.string.metric_identity, key.server, key.service, key.name), style = MachineTextStyle)
        }
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
    Card(Modifier.fillMaxWidth().clickable(onClick = onClick)) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name, style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.metric_identity, metric.server, metric.service, metric.name), style = MachineTextStyle)
            Text(stringResource(R.string.metric_origin, originLabel(metric.origin)))
            Text(metric.definition?.unit.orEmpty(), color = MaterialTheme.colorScheme.onSurfaceVariant)
            when {
                metric.definition?.enabled == false -> Text(stringResource(R.string.metric_definition_disabled))
                preview is DataLoadState.Loading -> Text(stringResource(R.string.metric_preview_loading), color = MaterialTheme.colorScheme.onSurfaceVariant)
                preview is DataLoadState.Error -> Row {
                    Text(stringResource(R.string.metric_preview_error), color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { retry++ }) { Text(stringResource(R.string.retry)) }
                }
                preview is DataLoadState.Content -> {
                    val chart = (preview as DataLoadState.Content).value
                    val latest = chart.series.firstOrNull()?.values?.asReversed()?.firstOrNull { it != null && it.isFinite() }
                    Text(
                        latest?.let { stringResource(R.string.metric_latest_value, number(it), chart.unit) }
                            ?: stringResource(R.string.metric_chart_empty),
                    )
                    if (chartScale(chart.series.flatMap { it.values }) != null) {
                        MetricChartCanvas(chart, compact = true)
                    }
                }
            }
        }
    }
}

@Composable
private fun LoadingMetricCatalog() {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        CircularProgressIndicator()
        Text(stringResource(R.string.metric_catalog_loading))
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
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

    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(stringResource(R.string.metric_detail_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = stringResource(R.string.back))
                    }
                },
                actions = {
                    TextButton(onClick = viewModel::retryMetric) { Text(stringResource(R.string.metric_refresh)) }
                },
            )
        },
    ) { insets ->
        DataStateContent(
            state = state.metric,
            isEmpty = { it == null },
            onRetry = viewModel::retryMetric,
            emptyText = stringResource(R.string.metric_missing),
            modifier = Modifier.fillMaxSize().padding(insets).padding(horizontal = 12.dp),
        ) { metric ->
            if (metric != null) {
                LazyColumn(
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                    modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
                ) {
                    item {
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(metric.definition?.title?.takeIf(String::isNotBlank) ?: metric.name, style = MaterialTheme.typography.titleLarge)
                            Text(stringResource(R.string.metric_identity, metric.server, metric.service, metric.name), style = MachineTextStyle)
                            Text(stringResource(R.string.metric_origin, originLabel(metric.origin)))
                            metric.definition?.description?.takeIf(String::isNotBlank)?.let { Text(it) }
                            metric.definition?.enabled?.let { if (!it) Text(stringResource(R.string.metric_definition_disabled)) }
                        }
                    }
                    item { MetricRangeSelector(state.range, viewModel::setRange) }
                    item { MetricSplitSelector(state.split, viewModel::setSplit) }
                    if (metric.environments.isNotEmpty()) {
                        item {
                            androidx.compose.foundation.layout.Box {
                                Button(onClick = { environmentMenu = true }) {
                                    Text(state.environment.ifBlank { stringResource(R.string.metric_environment_all) })
                                }
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
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                CircularProgressIndicator()
                                Text(stringResource(R.string.metric_chart_loading))
                            }
                        }
                        DataLoadState.Error -> item {
                            Column {
                                Text(stringResource(R.string.metric_chart_error), color = MaterialTheme.colorScheme.error)
                                TextButton(onClick = viewModel::retryChart) { Text(stringResource(R.string.retry)) }
                            }
                        }
                        is DataLoadState.Content -> {
                            item {
                                Text(
                                    stringResource(
                                        R.string.metric_chart_metadata,
                                        metricKind(chart.value.kind),
                                        aggregationLabel(chart.value.aggregation),
                                    ),
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            if (chartScale(chart.value.series.flatMap { it.values }) == null) {
                                item { Text(stringResource(R.string.metric_chart_empty), color = MaterialTheme.colorScheme.onSurfaceVariant) }
                            } else {
                                item { MetricSeriesChart(chart.value) }
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
    LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
        items(options, key = { it.first }) { (value, label) ->
            FilterChip(selected == value, onClick = { onSelect(value) }, label = { Text(stringResource(label)) })
        }
    }
}

@Composable
private fun MetricSeriesChart(chart: SeriesReply) {
    val scale = chartScale(chart.series.flatMap { it.values }) ?: return
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(stringResource(R.string.metric_axis_maximum, number(scale.maximum), chart.unit))
        }
        MetricChartCanvas(chart, compact = false)
        Text(stringResource(R.string.metric_axis_minimum, number(scale.minimum), chart.unit))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(chart.buckets.firstOrNull()?.let(::localTimestamp).orEmpty(), style = MaterialTheme.typography.labelSmall)
            Text(chart.buckets.lastOrNull()?.let(::localTimestamp).orEmpty(), style = MaterialTheme.typography.labelSmall)
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            chart.series.forEachIndexed { index, line ->
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    val color = chartColor(index)
                    Canvas(Modifier.padding(top = 7.dp).height(8.dp).fillMaxWidth(0.05f)) {
                        drawLine(color, Offset(0f, size.height / 2), Offset(size.width, size.height / 2), 3.dp.toPx())
                    }
                    Text(line.label.ifBlank { stringResource(R.string.metric_series_total) }, style = MaterialTheme.typography.labelSmall)
                }
            }
        }
        if (chart.buckets.isEmpty()) Text(stringResource(R.string.metric_chart_empty))
    }
}

@Composable
private fun MetricChartCanvas(chart: SeriesReply, compact: Boolean) {
    val scale = chartScale(chart.series.flatMap { it.values }) ?: return
    val colors = MaterialTheme.colorScheme
    val palette = listOf(colors.primary, colors.tertiary, colors.secondary, colors.error)
    Canvas(Modifier.fillMaxWidth().height(if (compact) 64.dp else 220.dp)) {
        val xPadding = if (compact) 1.dp.toPx() else 8.dp.toPx()
        val yPadding = if (compact) 2.dp.toPx() else 8.dp.toPx()
        val width = (size.width - xPadding * 2).coerceAtLeast(1f)
        val height = (size.height - yPadding * 2).coerceAtLeast(1f)
        if (!compact) {
            repeat(5) { step ->
                val y = yPadding + height * step / 4
                drawLine(colors.outlineVariant, Offset(xPadding, y), Offset(xPadding + width, y), 1.dp.toPx())
            }
        }
        chart.series.forEachIndexed { seriesIndex, series ->
            val path = Path()
            var previous = false
            val denominator = (series.values.size - 1).coerceAtLeast(1)
            series.values.forEachIndexed { index, value ->
                if (value == null || !value.isFinite()) {
                    previous = false
                } else {
                    val x = xPadding + width * index / denominator
                    val ratio = ((value - scale.minimum) / (scale.maximum - scale.minimum)).toFloat().coerceIn(0f, 1f)
                    val y = yPadding + height * (1f - ratio)
                    if (previous) path.lineTo(x, y) else path.moveTo(x, y)
                    previous = true
                }
            }
            drawPath(path, palette[seriesIndex % palette.size], style = androidx.compose.ui.graphics.drawscope.Stroke(width = if (compact) 1.7.dp.toPx() else 2.dp.toPx()))
        }
    }
}

@Composable
private fun chartColor(index: Int): Color = when (index % 4) {
    0 -> MaterialTheme.colorScheme.primary
    1 -> MaterialTheme.colorScheme.tertiary
    2 -> MaterialTheme.colorScheme.secondary
    else -> MaterialTheme.colorScheme.error
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

@Composable
private fun number(value: Double): String {
    val locale = androidx.compose.ui.platform.LocalConfiguration.current.locales[0]
    return NumberFormat.getNumberInstance(locale).apply { maximumFractionDigits = 3 }.format(value)
}

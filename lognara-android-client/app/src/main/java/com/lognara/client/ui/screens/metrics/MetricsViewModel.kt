package com.lognara.client.ui.screens.metrics

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.MetricFavoritesResponse
import com.lognara.client.data.MetricKey
import com.lognara.client.data.MetricLookupRequest
import com.lognara.client.data.MetricSource
import com.lognara.client.data.MetricSourcesResponse
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.SeriesMetricsResponse
import com.lognara.client.data.SeriesQuery
import com.lognara.client.data.SeriesReply
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import com.lognara.client.ui.screens.data.timeWindow
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.serialization.json.encodeToJsonElement

data class FavoriteMetric(val key: MetricKey, val metric: SeriesMetric?)

data class MetricsOverview(val favorites: List<FavoriteMetric>, val sources: List<MetricSource>)

data class MetricsUiState(val miniRange: DataRange = DataRange.TwentyFourHours)

fun seriesStepSeconds(range: DataRange): Long = when (range) {
    DataRange.FifteenMinutes -> 10
    DataRange.OneHour -> 60
    DataRange.TwentyFourHours -> 1_800
    DataRange.SevenDays -> 3_600
}

fun buildSeriesQuery(
    metric: SeriesMetric,
    window: TimeWindow,
    range: DataRange,
    split: String = "none",
    environment: String = "",
): SeriesQuery = SeriesQuery(
    server = metric.server,
    origin = metric.origin,
    service = metric.service,
    name = metric.name,
    from = window.from,
    to = window.to,
    step_seconds = seriesStepSeconds(range),
    split = split,
    environment = environment.takeIf(String::isNotBlank),
)

data class ChartScale(val minimum: Double, val maximum: Double)

fun chartScale(values: List<Double?>): ChartScale? {
    val finite = values.filterNotNull().filter(Double::isFinite)
    if (finite.isEmpty()) return null
    val minimum = finite.minOrNull() ?: return null
    val maximum = finite.maxOrNull() ?: return null
    if (minimum != maximum) return ChartScale(minimum, maximum)
    val padding = if (minimum == 0.0) 1.0 else kotlin.math.abs(minimum) * 0.05
    return ChartScale(minimum - padding, maximum + padding)
}

class MetricsViewModel(private val api: LognaraApi, private val session: DeviceSession) : ViewModel() {
    private val _overview = MutableStateFlow<DataLoadState<MetricsOverview>>(DataLoadState.Loading)
    val overview = _overview.asStateFlow()

    private val _selectedSource = MutableStateFlow<MetricSource?>(null)
    val selectedSource = _selectedSource.asStateFlow()

    private val _catalog = MutableStateFlow<DataLoadState<List<SeriesMetric>>>(DataLoadState.Content(emptyList()))
    val catalog = _catalog.asStateFlow()

    private val _state = MutableStateFlow(MetricsUiState())
    val state = _state.asStateFlow()

    private val previewSemaphore = Semaphore(2)
    private var overviewJob: Job? = null
    private var catalogJob: Job? = null

    init {
        reload()
    }

    fun setMiniRange(range: DataRange) {
        _state.value = _state.value.copy(miniRange = range)
    }

    fun selectSource(source: MetricSource?) {
        _selectedSource.value = source
        if (source == null) {
            catalogJob?.cancel()
            _catalog.value = DataLoadState.Content(emptyList())
        } else {
            loadCatalog(source)
        }
    }

    fun reload() {
        overviewJob?.cancel()
        catalogJob?.cancel()
        _overview.value = DataLoadState.Loading
        if (_selectedSource.value != null) _catalog.value = DataLoadState.Loading
        overviewJob = viewModelScope.launch {
            try {
                val favorites = api.authenticatedJson<MetricFavoritesResponse>(
                    session,
                    "GET",
                    DeviceApiRoutes.FAVORITES,
                ).keys
                val found = if (favorites.isEmpty()) {
                    emptyMap()
                } else {
                    api.authenticatedJson<SeriesMetricsResponse>(
                        session,
                        "POST",
                        DeviceApiRoutes.SERIES_LOOKUP,
                        apiJson.encodeToJsonElement(MetricLookupRequest.serializer(), MetricLookupRequest(favorites)),
                    ).metrics.associateBy(::metricIdentity)
                }
                val sources = api.authenticatedJson<MetricSourcesResponse>(
                    session,
                    "GET",
                    DeviceApiRoutes.SERIES_SOURCES,
                ).sources
                _overview.value = DataLoadState.Content(
                    MetricsOverview(
                        favorites = favorites.map { key -> FavoriteMetric(key, found[metricIdentity(key)]) },
                        sources = sources.sortedWith(compareBy(MetricSource::server, MetricSource::service)),
                    ),
                )
                _selectedSource.value?.let(::loadCatalog)
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _overview.value = DataLoadState.Error
            }
        }
    }

    fun reloadCatalog() {
        _selectedSource.value?.let(::loadCatalog)
    }

    suspend fun loadPreview(metric: SeriesMetric, range: DataRange): SeriesReply = previewSemaphore.withPermit {
        val window = timeWindow(range)
        val query = buildSeriesQuery(metric, window, range)
        api.authenticatedJson(
            session,
            "POST",
            DeviceApiRoutes.SERIES_QUERY,
            apiJson.encodeToJsonElement(SeriesQuery.serializer(), query),
        )
    }

    private fun loadCatalog(source: MetricSource) {
        catalogJob?.cancel()
        _catalog.value = DataLoadState.Loading
        catalogJob = viewModelScope.launch {
            try {
                val response = api.authenticatedJson<SeriesMetricsResponse>(
                    session,
                    "GET",
                    DeviceApiRoutes.seriesList(source.server, source.service),
                )
                _catalog.value = DataLoadState.Content(response.metrics)
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _catalog.value = DataLoadState.Error
            }
        }
    }
}

internal fun metricIdentity(metric: MetricKey): String = "${metric.server}\u0000${metric.service}\u0000${metric.name}\u0000${metric.origin}"
internal fun metricIdentity(metric: SeriesMetric): String = "${metric.server}\u0000${metric.service}\u0000${metric.name}\u0000${metric.origin}"

data class MetricDetailsUiState(
    val metric: DataLoadState<SeriesMetric?> = DataLoadState.Loading,
    val chart: DataLoadState<SeriesReply> = DataLoadState.Loading,
    val range: DataRange = DataRange.TwentyFourHours,
    val split: String = "none",
    val environment: String = "",
)

class MetricDetailsViewModel(
    private val api: LognaraApi,
    private val session: DeviceSession,
    private val key: MetricKey,
) : ViewModel() {
    private val _state = MutableStateFlow(MetricDetailsUiState())
    val state = _state.asStateFlow()
    private var metricJob: Job? = null
    private var chartJob: Job? = null

    init {
        loadMetric()
    }

    fun retryMetric() = loadMetric()

    fun retryChart() {
        val metric = (_state.value.metric as? DataLoadState.Content)?.value ?: return
        loadChart(metric)
    }

    fun setRange(range: DataRange) {
        if (_state.value.range == range) return
        _state.value = _state.value.copy(range = range)
        reloadForCurrentMetric()
    }

    fun setSplit(split: String) {
        if (_state.value.split == split) return
        _state.value = _state.value.copy(split = split)
        reloadForCurrentMetric()
    }

    fun setEnvironment(environment: String) {
        if (_state.value.environment == environment) return
        _state.value = _state.value.copy(environment = environment)
        reloadForCurrentMetric()
    }

    private fun reloadForCurrentMetric() {
        val metric = (_state.value.metric as? DataLoadState.Content)?.value
        if (metric != null) loadChart(metric)
    }

    private fun loadMetric() {
        metricJob?.cancel()
        chartJob?.cancel()
        _state.value = _state.value.copy(metric = DataLoadState.Loading, chart = DataLoadState.Loading)
        metricJob = viewModelScope.launch {
            try {
                val response = api.authenticatedJson<SeriesMetricsResponse>(
                    session,
                    "POST",
                    DeviceApiRoutes.SERIES_LOOKUP,
                    apiJson.encodeToJsonElement(MetricLookupRequest.serializer(), MetricLookupRequest(listOf(key))),
                )
                val metric = response.metrics.singleOrNull {
                    it.server == key.server && it.service == key.service && it.name == key.name && it.origin == key.origin
                }
                _state.value = _state.value.copy(metric = DataLoadState.Content(metric))
                metric?.let(::loadChart)
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = _state.value.copy(metric = DataLoadState.Error)
            }
        }
    }

    private fun loadChart(metric: SeriesMetric) {
        chartJob?.cancel()
        val current = _state.value
        val request = buildSeriesQuery(
            metric,
            timeWindow(current.range),
            current.range,
            split = current.split,
            environment = current.environment,
        )
        _state.value = current.copy(chart = DataLoadState.Loading)
        chartJob = viewModelScope.launch {
            try {
                val chart = api.authenticatedJson<SeriesReply>(
                    session,
                    "POST",
                    DeviceApiRoutes.SERIES_QUERY,
                    apiJson.encodeToJsonElement(SeriesQuery.serializer(), request),
                )
                _state.value = _state.value.copy(chart = DataLoadState.Content(chart))
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = _state.value.copy(chart = DataLoadState.Error)
            }
        }
    }
}

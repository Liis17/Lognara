package com.lognara.client.ui.screens.logs

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CenterAlignedTopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SearchResponse
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.MachineTextStyle
import java.util.concurrent.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun EventDetailScreen(event: LogEvent, onBack: () -> Unit, onOpenTrace: (LogEvent) -> Unit) {
    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(stringResource(R.string.event_detail_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = stringResource(R.string.back))
                    }
                },
            )
        },
    ) { insets ->
        Column(
            modifier = Modifier.fillMaxSize().padding(insets).verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            DetailField(stringResource(R.string.event_field_id), event.id)
            DetailField(stringResource(R.string.event_field_time), localTimestamp(event.timestamp))
            DetailField(stringResource(R.string.event_field_ingested), localTimestamp(event.ingested_at))
            DetailField(stringResource(R.string.event_field_level), stringResource(levelLabel(event.level)))
            DetailField(stringResource(R.string.event_field_service), event.service)
            DetailField(stringResource(R.string.event_field_instance), event.service_instance)
            DetailField(stringResource(R.string.event_field_server), event.server)
            DetailField(stringResource(R.string.event_field_backend), event.backend)
            DetailField(stringResource(R.string.event_field_environment), event.environment)
            DetailField(stringResource(R.string.event_field_action), event.action)
            DetailField(stringResource(R.string.event_field_trace), event.trace_id)
            DetailField(stringResource(R.string.event_field_span), event.span_id)
            DetailField(stringResource(R.string.event_field_parent_span), event.parent_span_id)
            DetailField(stringResource(R.string.event_field_request), event.request_id)
            DetailField(stringResource(R.string.event_message), event.message)
            DetailField(
                stringResource(R.string.event_field_attributes),
                event.attributes.entries.joinToString("\n") { (key, value) -> "$key: $value" }.ifEmpty { "—" },
            )
            if (event.trace_id != null) {
                TextButton(onClick = { onOpenTrace(event) }, modifier = Modifier.fillMaxWidth()) {
                    Text(stringResource(R.string.event_open_trace))
                }
            } else {
                Text(stringResource(R.string.trace_unavailable), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@Composable
private fun DetailField(label: String, value: String?) {
    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value ?: "—", style = MachineTextStyle)
    }
}

class TraceDetailViewModel(
    private val api: LognaraApi,
    private val session: DeviceSession,
    private val traceId: String,
    private val from: String,
    private val to: String,
) : ViewModel() {
    private val _state = MutableStateFlow<DataLoadState<SearchResponse>>(DataLoadState.Loading)
    val state = _state.asStateFlow()

    init {
        load()
    }

    fun retry() = load()

    private fun load() {
        _state.value = DataLoadState.Loading
        viewModelScope.launch {
            try {
                val response = api.authenticatedJson<SearchResponse>(
                    session,
                    "GET",
                    DeviceApiRoutes.trace(traceId, from, to),
                )
                _state.value = DataLoadState.Content(response)
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: ApiException.Unauthorized) {
                return@launch
            } catch (_: Exception) {
                _state.value = DataLoadState.Error
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun TraceDetailScreen(
    api: LognaraApi,
    session: DeviceSession,
    traceId: String,
    from: String,
    to: String,
    onBack: () -> Unit,
    onOpenEvent: (LogEvent) -> Unit,
) {
    val viewModel: TraceDetailViewModel = viewModel(key = "trace-${session.deviceId}-$traceId-$from-$to") {
        TraceDetailViewModel(api, session, traceId, from, to)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    Scaffold(
        topBar = {
            CenterAlignedTopAppBar(
                title = { Text(stringResource(R.string.trace_detail_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = stringResource(R.string.back))
                    }
                },
            )
        },
    ) { insets ->
        DataStateContent(
            state = state,
            isEmpty = { it.events.isEmpty() },
            onRetry = viewModel::retry,
            emptyText = stringResource(R.string.trace_empty),
            modifier = Modifier.fillMaxSize().padding(insets).padding(horizontal = 12.dp),
        ) { response ->
            LazyColumn(Modifier.fillMaxSize().padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                items(response.events, key = { it.id }) { event ->
                    LogEventRow(event = event, onClick = { onOpenEvent(event) })
                }
            }
        }
    }
}

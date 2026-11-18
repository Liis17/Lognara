package com.lognara.client.ui.screens.logs

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SearchResponse
import com.lognara.client.data.apiJson
import com.lognara.client.ui.components.DetailTopBar
import com.lognara.client.ui.components.LevelBadge
import com.lognara.client.ui.components.LogEventRow
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.PillButton
import com.lognara.client.ui.components.RoundIconButton
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.SegmentedSelector
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.LognaraPalette
import java.util.concurrent.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

private enum class AttributeView { Table, Json }
private val attributesJson = Json { prettyPrint = true }

@Composable
fun EventDetailScreen(event: LogEvent, onBack: () -> Unit, onOpenTrace: (LogEvent) -> Unit) {
    val context = LocalContext.current
    var attributeView by rememberSaveable { mutableStateOf(AttributeView.Table) }
    val jsonAttributes = remember(event.attributes) {
        attributesJson.encodeToString(JsonObject.serializer(), JsonObject(event.attributes))
    }
    val fields = eventFields(event)
    val hiddenFields = hiddenEventFields(event)

    Column(Modifier.fillMaxSize().background(LognaraPalette.Background)) {
        DetailTopBar(
            title = stringResource(R.string.event_detail_title),
            onBack = onBack,
            actions = {
                RoundIconButton(
                    icon = R.drawable.ic_copy,
                    contentDescription = stringResource(R.string.event_copy_json),
                    onClick = {
                        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        val eventJson = apiJson.encodeToString(LogEvent.serializer(), event)
                        clipboard.setPrimaryClip(ClipData.newPlainText("event-json", eventJson))
                    },
                )
            },
        )
        Column(
            modifier = Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                LevelBadge(event.level)
                Text(localTimestamp(event.timestamp), style = monoStyle(12), color = LognaraPalette.TextMuted)
                Text(
                    event.service,
                    fontSize = 13.sp,
                    color = LognaraPalette.TextSecondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }

            Box(
                Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(8.dp))
                    .background(LognaraPalette.Background)
                    .border(1.dp, LognaraPalette.BorderStrong, RoundedCornerShape(8.dp))
                    .padding(horizontal = 14.dp, vertical = 12.dp),
            ) {
                Text(event.message, style = monoStyle(14), color = LognaraPalette.Text)
            }

            if (event.trace_id != null) {
                PillButton(
                    text = stringResource(R.string.event_open_trace),
                    onClick = { onOpenTrace(event) },
                    modifier = Modifier.fillMaxWidth().height(44.dp),
                    icon = R.drawable.ic_node,
                )
            }

            LognaraCard(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 4.dp),
                verticalArrangement = Arrangement.spacedBy(0.dp),
            ) {
                fields.forEachIndexed { index, field ->
                    DetailFieldRow(label = field.first, value = field.second)
                    if (index < fields.lastIndex) RowDivider()
                }
            }

            if (hiddenFields.isNotEmpty()) {
                Text(
                    stringResource(R.string.event_hidden_fields, hiddenFields.joinToString(", ")),
                    modifier = Modifier.padding(horizontal = 4.dp),
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                    color = LognaraPalette.TextMuted,
                )
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(stringResource(R.string.event_field_attributes), fontSize = 16.sp, color = LognaraPalette.Text)
                    Text(event.backend, style = monoStyle(13), color = LognaraPalette.TextMuted)
                }
                SegmentedSelector(
                    options = AttributeView.entries,
                    selected = attributeView,
                    label = { view ->
                        stringResource(if (view == AttributeView.Table) R.string.event_attributes_table else R.string.event_attributes_json)
                    },
                    onSelect = { attributeView = it },
                    modifier = Modifier.width(184.dp),
                    itemHeight = 30.dp,
                )
            }

            if (attributeView == AttributeView.Json) {
                LognaraCard(contentPadding = PaddingValues(14.dp), verticalArrangement = Arrangement.spacedBy(0.dp)) {
                    Text(
                        jsonAttributes.ifBlank { "{}" },
                        style = monoStyle(12),
                        color = LognaraPalette.TextSecondary,
                    )
                }
            } else {
                AttributeTable(event.attributes)
            }
        }
    }
}

private data class EventField(val first: String, val second: String)

@Composable
private fun eventFields(event: LogEvent): List<EventField> = buildList {
    add(EventField(stringResource(R.string.event_field_time), localTimestamp(event.timestamp)))
    add(EventField(stringResource(R.string.event_field_ingested), localTimestamp(event.ingested_at)))
    add(EventField(stringResource(R.string.event_field_service), event.service))
    event.service_instance?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_instance), it))
    }
    add(EventField(stringResource(R.string.event_field_server), event.server))
    add(EventField(stringResource(R.string.event_field_backend), event.backend))
    event.environment?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_environment), it))
    }
    event.action?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_action), it))
    }
    event.trace_id?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_trace), it))
    }
    event.span_id?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_span), it))
    }
    event.parent_span_id?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_parent_span), it))
    }
    event.request_id?.takeIf(String::isNotBlank)?.let {
        add(EventField(stringResource(R.string.event_field_request), it))
    }
    add(EventField(stringResource(R.string.event_field_id), event.id))
}

@Composable
private fun hiddenEventFields(event: LogEvent): List<String> = buildList {
    if (event.service_instance.isNullOrBlank()) add(stringResource(R.string.event_field_instance))
    if (event.environment.isNullOrBlank()) add(stringResource(R.string.event_field_environment))
    if (event.action.isNullOrBlank()) add(stringResource(R.string.event_field_action))
    if (event.trace_id.isNullOrBlank()) add(stringResource(R.string.event_field_trace))
    if (event.span_id.isNullOrBlank()) add(stringResource(R.string.event_field_span))
    if (event.parent_span_id.isNullOrBlank()) add(stringResource(R.string.event_field_parent_span))
    if (event.request_id.isNullOrBlank()) add(stringResource(R.string.event_field_request))
}

@Composable
private fun DetailFieldRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 9.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, modifier = Modifier.weight(0.9f), fontSize = 13.sp, color = LognaraPalette.TextMuted)
        Text(
            value,
            modifier = Modifier.weight(1.1f),
            style = monoStyle(12),
            color = LognaraPalette.Text,
            textAlign = TextAlign.End,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun AttributeTable(attributes: Map<String, kotlinx.serialization.json.JsonElement>) {
    if (attributes.isEmpty()) {
        Text(
            stringResource(R.string.event_attributes_empty),
            color = LognaraPalette.TextMuted,
            modifier = Modifier.padding(horizontal = 4.dp, vertical = 8.dp),
        )
        return
    }
    val groups = attributes.toSortedMap().entries.groupBy { attributeGroup(it.key) }
    val groupOrder = listOf("Counters", "Gauges", "Атрибуты")
    val visibleGroups = groupOrder.mapNotNull { name -> groups[name]?.let { name to it } }
    LognaraCard(
        contentPadding = PaddingValues(0.dp),
        verticalArrangement = Arrangement.spacedBy(0.dp),
    ) {
        visibleGroups.forEachIndexed { groupIndex, (name, entries) ->
            Box(
                Modifier.fillMaxWidth().background(LognaraPalette.Text.copy(alpha = 0.02f)).padding(horizontal = 14.dp, vertical = 8.dp),
            ) {
                SectionLabel(name)
            }
            RowDivider()
            entries.forEachIndexed { index, entry ->
                AttributeRow(key = entry.key, value = attributeText(entry.value))
                if (index < entries.lastIndex) RowDivider()
            }
            if (groupIndex < visibleGroups.lastIndex) RowDivider()
        }
    }
}

private fun attributeGroup(key: String): String = when {
    key.startsWith("disk.") || key.startsWith("net.") -> "Counters"
    key.startsWith("cpu.") || key.startsWith("fs.") -> "Gauges"
    else -> "Атрибуты"
}

private fun attributeText(value: kotlinx.serialization.json.JsonElement): String =
    runCatching { value.jsonPrimitive.contentOrNull }.getOrNull() ?: value.toString()

@Composable
private fun AttributeRow(key: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 7.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            key,
            modifier = Modifier.weight(1f),
            style = monoStyle(12),
            color = LognaraPalette.TextSecondary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            value,
            modifier = Modifier.weight(0.8f),
            style = monoStyle(12),
            color = LognaraPalette.Text,
            textAlign = TextAlign.End,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
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

    Column(Modifier.fillMaxSize().background(LognaraPalette.Background)) {
        DetailTopBar(title = stringResource(R.string.logs_trace_title), onBack = onBack)
        when (val result = state) {
            DataLoadState.Loading -> DetailState(
                loading = true,
                text = stringResource(R.string.data_loading),
                modifier = Modifier.fillMaxSize(),
            )
            DataLoadState.Error -> DetailState(
                text = stringResource(R.string.data_load_error),
                actionText = stringResource(R.string.retry),
                onAction = viewModel::retry,
                modifier = Modifier.fillMaxSize(),
            )
            is DataLoadState.Content -> if (result.value.events.isEmpty()) {
                DetailState(
                    text = stringResource(R.string.trace_empty),
                    modifier = Modifier.fillMaxSize(),
                )
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    item {
                        LognaraCard(
                            contentPadding = PaddingValues(0.dp),
                            verticalArrangement = Arrangement.spacedBy(0.dp),
                        ) {
                            result.value.events.forEachIndexed { index, event ->
                                if (index > 0) RowDivider()
                                TraceEventRow(event = event, onClick = { onOpenEvent(event) })
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun DetailState(
    text: String,
    modifier: Modifier = Modifier,
    loading: Boolean = false,
    actionText: String? = null,
    onAction: (() -> Unit)? = null,
) {
    Box(modifier.padding(16.dp), contentAlignment = Alignment.Center) {
        LognaraCard(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                if (loading) CircularProgressIndicator(
                    modifier = Modifier.size(22.dp),
                    color = LognaraPalette.Accent,
                    strokeWidth = 2.dp,
                )
                Text(text, color = LognaraPalette.TextMuted)
            }
            if (actionText != null && onAction != null) {
                TextButton(onClick = onAction) { Text(actionText) }
            }
        }
    }
}

@Composable
private fun TraceEventRow(event: LogEvent, onClick: () -> Unit) {
    LogEventRow(
        level = event.level,
        time = localTimestamp(event.timestamp).substringAfter(' ', localTimestamp(event.timestamp)),
        service = event.service,
        server = event.server,
        message = event.message,
        onClick = onClick,
    )
}

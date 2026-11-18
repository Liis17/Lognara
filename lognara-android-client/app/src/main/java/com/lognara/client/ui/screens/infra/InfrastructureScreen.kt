package com.lognara.client.ui.screens.infra

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lognara.client.R
import com.lognara.client.data.CoreStatus
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.DockStatus
import com.lognara.client.data.InfrastructureNode
import com.lognara.client.data.InfrastructureResponse
import com.lognara.client.data.InfrastructureStorage
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.SpoolUsage
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.RoundIconButton
import com.lognara.client.ui.components.ScreenHeader
import com.lognara.client.ui.components.SettingsButton
import com.lognara.client.ui.components.StatValue
import com.lognara.client.ui.components.StatusPill
import com.lognara.client.ui.components.UsageBar
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.formatDataNumber
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun InfrastructureScreen(api: LognaraApi, session: DeviceSession, onSettings: () -> Unit) {
    val viewModel: InfrastructureViewModel = viewModel(key = "infrastructure-${session.deviceId}") {
        InfrastructureViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val snapshot = when (val current = state) {
        is DataLoadState.Content -> localTimestamp(current.value.as_of)
        else -> null
    }

    Column(Modifier.fillMaxSize()) {
        ScreenHeader(
            title = stringResource(R.string.tab_infrastructure),
            subtitleContent = {
                snapshot?.let {
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(stringResource(R.string.infra_ui_snapshot), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                        Text(it, style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1)
                    }
                }
            },
            actions = {
                RoundIconButton(
                    icon = R.drawable.ic_refresh,
                    contentDescription = stringResource(R.string.infra_ui_refresh),
                    onClick = viewModel::reload,
                )
                SettingsButton(onSettings)
            },
        )
        Box(Modifier.weight(1f)) {
            PullToRefreshBox(
                isRefreshing = state is DataLoadState.Loading,
                onRefresh = viewModel::reload,
                modifier = Modifier.fillMaxSize(),
            ) {
                DataStateContent(
                    state = state,
                    isEmpty = { false },
                    onRetry = viewModel::reload,
                    modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
                ) { response ->
                    InfrastructureContent(response)
                }
            }
        }
    }
}

@Composable
private fun InfrastructureContent(response: InfrastructureResponse) {
    val groups = groupInfrastructureNodes(response.nodes)
    LazyColumn(
        contentPadding = PaddingValues(bottom = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxSize(),
    ) {
        item(key = "service-cards") {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                CoreCard(response.core, Modifier.weight(1f))
                DockCard(response.dock, Modifier.weight(1f))
            }
        }
        item(key = "storage") { StorageCard(response.storage) }
        item(key = "nodes-heading") {
            Row(
                modifier = Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(stringResource(R.string.infra_nodes), fontSize = 16.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
                Text(
                    stringResource(
                        R.string.infra_ui_nodes_summary,
                        response.nodes.count { it.kind == "relay" },
                        response.nodes.count { it.kind == "agent" },
                    ),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                )
            }
        }
        if (!response.registry) {
            item(key = "registry-unavailable") {
                Text(
                    stringResource(R.string.infra_registry_unavailable),
                    fontSize = 13.sp,
                    color = LognaraPalette.TextMuted,
                    modifier = Modifier.padding(horizontal = 4.dp),
                )
            }
        } else if (groups.isEmpty()) {
            item(key = "nodes-empty") {
                Text(
                    stringResource(R.string.infra_nodes_empty),
                    fontSize = 13.sp,
                    color = LognaraPalette.TextMuted,
                    modifier = Modifier.padding(horizontal = 4.dp),
                )
            }
        } else {
            groups.forEach { group ->
                item(key = "relay-${group.relayId ?: group.relayName ?: "none"}") {
                    RelayGroupCard(group, response.as_of, response.thresholds.spool_warn_percent)
                }
            }
        }
    }
}

@Composable
private fun CoreCard(core: CoreStatus, modifier: Modifier) {
    val statusColor = coreStatusColor(core.status)
    LognaraCard(
        modifier = modifier,
        contentPadding = PaddingValues(14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                stringResource(R.string.infra_core),
                modifier = Modifier.weight(1f),
                fontSize = 15.sp,
                fontWeight = FontWeight.W600,
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            StatusPill(stringResource(coreStatusLabel(core.status)), statusColor)
        }
        (core.release_version ?: core.version ?: core.git_commit)?.let {
            Text(it, style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        core.visibility_lag_ms?.let {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(stringResource(R.string.infra_ui_publication_delay), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                Text(
                    stringResource(R.string.infra_ui_seconds, number(it / 1000.0)),
                    fontSize = 18.sp,
                    fontWeight = FontWeight.W600,
                    color = LognaraPalette.Text,
                )
            }
        }
        val walBytes = core.wal_bytes
        val walLimitBytes = core.wal_limit_bytes
        if (walBytes != null && walLimitBytes != null) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    stringResource(R.string.infra_ui_wal_line, byteValue(walBytes), byteValue(walLimitBytes)),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (walLimitBytes > 0) {
                    val fraction = walBytes.toDouble().div(walLimitBytes).toFloat().coerceIn(0f, 1f)
                    val description = stringResource(R.string.infra_ui_wal_line, byteValue(walBytes), byteValue(walLimitBytes))
                    UsageBar(
                        fraction = fraction,
                        color = LognaraPalette.Accent,
                        modifier = Modifier.semantics {
                            contentDescription = description
                            progressBarRangeInfo = ProgressBarRangeInfo(fraction, 0f..1f)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun DockCard(dock: DockStatus, modifier: Modifier) {
    LognaraCard(
        modifier = modifier,
        contentPadding = PaddingValues(14.dp),
        shape = RoundedCornerShape(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                stringResource(R.string.infra_dock),
                modifier = Modifier.weight(1f),
                fontSize = 15.sp,
                fontWeight = FontWeight.W600,
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            // Успешный снимок пришёл от Dock, поэтому он отвечает на текущий запрос.
            StatusPill(stringResource(R.string.infra_status_online), LognaraSemanticColors.statusOk)
        }
        Text(
            dock.release_version ?: dock.version,
            style = monoStyle(12),
            color = LognaraPalette.TextMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        dock.sessions?.let {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(stringResource(R.string.infra_ui_sessions), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                Text(number(it), fontSize = 18.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.infra_ui_sse_clients), fontSize = 12.sp, color = LognaraPalette.TextMuted)
            Text(number(dock.sse_clients), fontSize = 18.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
        }
    }
}

@Composable
private fun StorageCard(storage: InfrastructureStorage?) {
    LognaraCard {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                stringResource(R.string.infra_storage),
                modifier = Modifier.weight(1f),
                fontSize = 15.sp,
                fontWeight = FontWeight.W600,
                color = LognaraPalette.Text,
            )
            if (storage != null && storage.disk_total_bytes > 0) {
                val total = storage.disk_total_bytes
                val used = (total - storage.disk_free_bytes).coerceAtLeast(0)
                Text(
                    stringResource(R.string.infra_ui_storage_occupied, number(used.toDouble() * 100 / total)),
                    fontSize = 12.sp,
                    color = LognaraPalette.TextMuted,
                    maxLines = 1,
                )
            }
        }
        if (storage == null) {
            Text(stringResource(R.string.infra_storage_unavailable), fontSize = 13.sp, color = LognaraPalette.TextMuted)
        } else {
            val total = storage.disk_total_bytes
            val free = storage.disk_free_bytes
            if (total > 0) {
                val used = (total - free).coerceAtLeast(0)
                val fraction = (used.toDouble() / total).toFloat().coerceIn(0f, 1f)
                val description = stringResource(R.string.infra_ui_storage_occupied, number(fraction * 100))
                Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(byteValue(used), fontSize = 26.sp, lineHeight = 33.sp, fontWeight = FontWeight.W600, color = LognaraPalette.Text)
                    Text(
                        stringResource(R.string.infra_ui_storage_value_suffix, byteValue(total)),
                        fontSize = 13.sp,
                        color = LognaraPalette.TextMuted,
                    )
                }
                UsageBar(
                    fraction = fraction,
                    color = LognaraPalette.Accent,
                    modifier = Modifier.semantics {
                        contentDescription = description
                        progressBarRangeInfo = ProgressBarRangeInfo(fraction, 0f..1f)
                    },
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                StatValue(
                    stringResource(R.string.infra_ui_retention),
                    stringResource(R.string.infra_ui_retention_days, number(storage.retention_seconds / 86_400.0)),
                    modifier = Modifier.weight(1f),
                )
                StatValue(stringResource(R.string.infra_ui_segments), number(storage.segments), modifier = Modifier.weight(1f))
                StatValue(stringResource(R.string.infra_ui_free), byteValue(free), modifier = Modifier.weight(1f))
            }
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                StatValue(stringResource(R.string.infra_ui_parquet), byteValue(storage.parquet_bytes), modifier = Modifier.weight(1f))
                StatValue(stringResource(R.string.infra_ui_indexes), byteValue(storage.index_bytes), modifier = Modifier.weight(1f))
                val oldestDate = storage.oldest_event_at?.let(::localDate)
                if (oldestDate != null) {
                    StatValue(stringResource(R.string.infra_ui_since), oldestDate, modifier = Modifier.weight(1f))
                } else {
                    Spacer(Modifier.weight(1f))
                }
            }
        }
    }
}

@Composable
private fun RelayGroupCard(group: InfrastructureNodeGroup, asOf: String, spoolWarnPercent: Long) {
    val relay = group.nodes.firstOrNull { it.kind == "relay" }
    val standaloneHost = group.nodes.firstOrNull { it.kind == "host" }
    val statusNode = relay ?: standaloneHost
    val statusDescription = statusNode?.let { stringResource(nodeStatusLabel(it.status)) }
        ?: stringResource(R.string.infra_status_unknown)
    val title = relay?.name ?: group.relayName
        ?: stringResource(R.string.infra_ui_unassigned_group)

    LognaraCard(
        contentPadding = PaddingValues(0.dp),
        verticalArrangement = Arrangement.spacedBy(0.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().background(LognaraPalette.SurfaceRaised).padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(
                painter = painterResource(R.drawable.ic_node),
                contentDescription = null,
                tint = LognaraSemanticColors.statusInfo,
                modifier = Modifier.size(18.dp),
            )
            Text(
                title,
                modifier = Modifier.weight(1f),
                fontSize = 14.sp,
                fontWeight = FontWeight.W600,
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Box(
                Modifier
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(statusNode?.let { nodeStatusColor(it.status) } ?: LognaraSemanticColors.statusWarn)
                    .semantics { contentDescription = statusDescription },
            )
        }
        RowDivider()
        val detailNodes = buildList {
            relay?.let(::add)
            addAll(group.nodes.filter { it.kind != "relay" })
        }
        detailNodes.forEachIndexed { index, node ->
            if (index > 0) RowDivider()
            InfrastructureNodeContent(node, asOf, spoolWarnPercent)
        }
    }
}

@Composable
private fun InfrastructureNodeContent(node: InfrastructureNode, asOf: String, spoolWarnPercent: Long) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResource(nodeKindLabel(node.kind), node.name),
                modifier = Modifier.weight(1f),
                style = monoStyle(13, FontWeight.W500),
                color = LognaraPalette.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            StatusPill(stringResource(nodeStatusLabel(node.status)), nodeStatusColor(node.status))
        }
        node.reason?.let { reason ->
            val message = if (reason == "spool") {
                stringResource(R.string.infra_reason_spool, spoolWarnPercent)
            } else {
                stringResource(nodeReasonLabel(reason))
            }
            Text(message, fontSize = 12.sp, color = LognaraPalette.TextMuted)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            StatValue(
                stringResource(R.string.infra_ui_heartbeat),
                ageLabel(node.heartbeat_at, asOf),
                modifier = Modifier.weight(1f),
            )
            StatValue(
                stringResource(R.string.infra_ui_last_event),
                ageLabel(node.last_event_at, asOf),
                modifier = Modifier.weight(1f),
            )
            StatValue(
                stringResource(R.string.infra_ui_failures),
                node.delivery_failures?.let { number(it) } ?: stringResource(R.string.infra_no_data),
                modifier = Modifier.weight(1f),
            )
        }
        node.spool?.let { SpoolUsageContent(it, spoolWarnPercent) }
        val metadata = listOfNotNull(
            node.source?.service?.takeIf(String::isNotBlank),
            node.source?.server?.takeIf(String::isNotBlank),
            (node.release_version ?: node.version)?.takeIf(String::isNotBlank),
        ).joinToString(" · ")
        if (metadata.isNotEmpty()) {
            Text(metadata, style = monoStyle(12), color = LognaraPalette.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        node.events_per_second?.let {
            Text(
                stringResource(R.string.infra_events_rate, number(it)),
                fontSize = 12.sp,
                color = LognaraPalette.TextMuted,
            )
        }
    }
}

@Composable
private fun SpoolUsageContent(spool: SpoolUsage, warnPercent: Long) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                stringResource(
                    R.string.infra_ui_spool_line,
                    byteValue(spool.used_bytes),
                    byteValue(spool.limit_bytes),
                    number(spool.records),
                ),
                fontSize = 12.sp,
                color = LognaraPalette.TextMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(
                stringResource(R.string.infra_ui_spool_threshold, warnPercent),
                fontSize = 12.sp,
                color = LognaraPalette.TextMuted,
                maxLines = 1,
            )
        }
        if (spool.limit_bytes > 0) {
            val fraction = (spool.used_bytes.toDouble() / spool.limit_bytes).toFloat().coerceIn(0f, 1f)
            val description = stringResource(
                R.string.infra_ui_spool_line,
                byteValue(spool.used_bytes),
                byteValue(spool.limit_bytes),
                number(spool.records),
            )
            UsageBar(
                fraction = fraction,
                color = LognaraPalette.Accent,
                modifier = Modifier.semantics {
                    contentDescription = description
                    progressBarRangeInfo = ProgressBarRangeInfo(fraction, 0f..1f)
                },
            )
        }
    }
}

@Composable
private fun ageLabel(value: String?, asOf: String): String {
    if (value == null) return stringResource(R.string.infra_no_data)
    val elapsed = runCatching { Duration.between(Instant.parse(value), Instant.parse(asOf)).seconds }.getOrNull()
        ?: return stringResource(R.string.infra_no_data)
    val seconds = elapsed.coerceAtLeast(0)
    return when {
        seconds < 1 -> stringResource(R.string.infra_age_under_second)
        seconds < 60 -> stringResource(R.string.infra_age_seconds, seconds)
        seconds < 3_600 -> stringResource(R.string.infra_age_minutes, seconds / 60)
        seconds < 86_400 -> stringResource(R.string.infra_age_hours, seconds / 3_600)
        else -> stringResource(R.string.infra_age_days, seconds / 86_400)
    }
}

@Composable
private fun byteValue(value: Long): String {
    val unitResources = listOf(R.string.unit_bytes, R.string.unit_kilobytes, R.string.unit_megabytes, R.string.unit_gigabytes, R.string.unit_terabytes)
    var amount = value.toDouble()
    var unit = 0
    while (amount >= 1024 && unit < unitResources.lastIndex) {
        amount /= 1024
        unit++
    }
    return stringResource(R.string.infra_byte_value, number(amount), stringResource(unitResources[unit]))
}

@Composable
private fun number(value: Number): String = formatDataNumber(value, Locale.forLanguageTag("ru-RU"))

private fun localDate(value: String): String? = runCatching {
    DateTimeFormatter.ofPattern("dd.MM", Locale.forLanguageTag("ru-RU"))
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrNull()

private fun coreStatusLabel(status: String): Int = when (status) {
    "online" -> R.string.infra_status_online
    "not_ready" -> R.string.infra_status_not_ready
    else -> R.string.infra_status_unavailable
}

private fun nodeStatusLabel(status: String): Int = when (status) {
    "online" -> R.string.infra_status_online
    "idle" -> R.string.infra_status_idle
    "offline" -> R.string.infra_status_offline
    "degraded" -> R.string.infra_status_degraded
    else -> R.string.infra_status_unknown
}

private fun nodeKindLabel(kind: String): Int = when (kind) {
    "relay" -> R.string.infra_node_relay
    "host" -> R.string.infra_node_host
    else -> R.string.infra_node_agent
}

private fun nodeReasonLabel(reason: String): Int = when (reason) {
    "no_heartbeat" -> R.string.infra_reason_no_heartbeat
    "relay_silent" -> R.string.infra_reason_relay_silent
    "spool" -> R.string.infra_reason_spool
    else -> R.string.infra_reason_delivery
}

@Composable
private fun coreStatusColor(status: String): Color = when (status) {
    "online" -> LognaraSemanticColors.statusOk
    "not_ready" -> LognaraSemanticColors.statusWarn
    else -> LognaraSemanticColors.statusErr
}

@Composable
private fun nodeStatusColor(status: String): Color = when (status) {
    "online" -> LognaraSemanticColors.statusOk
    "idle" -> LognaraSemanticColors.statusInfo
    "degraded", "unknown", "not_ready" -> LognaraSemanticColors.statusWarn
    else -> LognaraSemanticColors.statusErr
}

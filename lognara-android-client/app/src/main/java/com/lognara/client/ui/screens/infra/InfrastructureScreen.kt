package com.lognara.client.ui.screens.infra

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
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
import com.lognara.client.ui.screens.data.DataLoadState
import com.lognara.client.ui.screens.data.DataStateContent
import com.lognara.client.ui.screens.data.formatDataNumber
import com.lognara.client.ui.screens.data.localTimestamp
import com.lognara.client.ui.theme.MachineTextStyle
import java.time.Duration
import java.time.Instant

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun InfrastructureScreen(api: LognaraApi, session: DeviceSession) {
    val viewModel: InfrastructureViewModel = viewModel(key = "infrastructure-${session.deviceId}") {
        InfrastructureViewModel(api, session)
    }
    val state by viewModel.state.collectAsStateWithLifecycle()

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
            InfrastructureContent(response, viewModel::reload)
        }
    }
}

@Composable
private fun InfrastructureContent(response: InfrastructureResponse, onRefresh: () -> Unit) {
    val groups = groupInfrastructureNodes(response.nodes)
    LazyColumn(
        verticalArrangement = Arrangement.spacedBy(10.dp),
        modifier = Modifier.fillMaxSize(),
    ) {
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(stringResource(R.string.infra_updated, localTimestamp(response.as_of)), style = MaterialTheme.typography.labelMedium)
                Button(onClick = onRefresh) { Text(stringResource(R.string.issue_refresh)) }
            }
        }
        item { CoreCard(response.core) }
        item { DockCard(response.dock) }
        item { StorageCard(response.storage) }
        item { Text(stringResource(R.string.infra_nodes), style = MaterialTheme.typography.titleLarge) }
        if (!response.registry) {
            item { Text(stringResource(R.string.infra_registry_unavailable), color = MaterialTheme.colorScheme.error) }
        } else if (groups.isEmpty()) {
            item { Text(stringResource(R.string.infra_nodes_empty), color = MaterialTheme.colorScheme.onSurfaceVariant) }
        } else {
            groups.forEach { group ->
                item(key = "relay-${group.relayId ?: group.relayName ?: "none"}") {
                    Text(
                        text = group.relayName?.let { stringResource(R.string.infra_relay_group, it) }
                            ?: stringResource(R.string.infra_relay_unassigned),
                        style = MaterialTheme.typography.titleMedium,
                    )
                }
                items(group.nodes, key = { "${it.kind}-${it.id}" }) { node ->
                    InfrastructureNodeCard(node, response.as_of, response.thresholds.spool_warn_percent)
                }
            }
        }
        item {
            Text(
                stringResource(
                    R.string.infra_thresholds,
                    response.thresholds.heartbeat_ttl_seconds,
                    response.thresholds.idle_seconds,
                    response.thresholds.spool_warn_percent,
                ),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun CoreCard(core: CoreStatus) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(stringResource(R.string.infra_core), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(coreStatusLabel(core.status)), color = statusColor(core.status))
            Text(stringResource(R.string.infra_version, core.release_version ?: core.version ?: core.git_commit ?: "—"))
            core.visibility_lag_ms?.let {
                Text(stringResource(R.string.infra_visibility_lag, number(it / 1000.0)))
            }
            Text(
                stringResource(
                    R.string.infra_wal,
                    core.wal_bytes?.let { byteValue(it) } ?: stringResource(R.string.infra_no_data),
                    core.wal_limit_bytes?.let { byteValue(it) } ?: stringResource(R.string.infra_no_data),
                ),
            )
        }
    }
}

@Composable
private fun DockCard(dock: DockStatus) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(stringResource(R.string.infra_dock), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.infra_version, dock.release_version ?: dock.version))
            Text(stringResource(R.string.infra_sse_clients, dock.sse_clients))
            Text(stringResource(R.string.infra_sessions, dock.sessions?.let { number(it) } ?: stringResource(R.string.infra_no_data)))
        }
    }
}

@Composable
private fun StorageCard(storage: InfrastructureStorage?) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(stringResource(R.string.infra_storage), style = MaterialTheme.typography.titleMedium)
            if (storage == null) {
                Text(stringResource(R.string.infra_storage_unavailable), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                val used = (storage.disk_total_bytes - storage.disk_free_bytes).coerceAtLeast(0)
                Text(stringResource(R.string.infra_storage_usage, byteValue(used), byteValue(storage.disk_total_bytes)))
                Text(stringResource(R.string.infra_storage_free, byteValue(storage.disk_free_bytes)))
                Text(stringResource(R.string.infra_storage_segments, number(storage.segments)))
                Text(stringResource(R.string.infra_storage_parts, byteValue(storage.parquet_bytes), byteValue(storage.index_bytes)))
                Text(stringResource(R.string.infra_retention, number(storage.retention_seconds / 86_400.0)))
                Text(
                    storage.oldest_event_at?.let {
                        stringResource(R.string.infra_oldest_event, localTimestamp(it))
                    } ?: stringResource(R.string.infra_no_events),
                )
            }
        }
    }
}

@Composable
private fun InfrastructureNodeCard(node: InfrastructureNode, asOf: String, spoolWarnPercent: Long) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResource(nodeKindLabel(node.kind), node.name),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(stringResource(nodeStatusLabel(node.status)), color = statusColor(node.status))
            node.reason?.let { reason ->
                val message = if (reason == "spool") {
                    stringResource(R.string.infra_reason_spool, spoolWarnPercent)
                } else {
                    stringResource(nodeReasonLabel(reason))
                }
                Text(message)
            }
            Text(stringResource(R.string.infra_heartbeat, ageLabel(node.heartbeat_at, asOf)))
            Text(stringResource(R.string.infra_last_event, ageLabel(node.last_event_at, asOf)))
            node.version?.let { version ->
                Text(stringResource(R.string.infra_version, node.release_version ?: version))
            }
            node.spool?.let { Text(spoolLabel(it, spoolWarnPercent)) }
            node.delivery_failures?.let { Text(stringResource(R.string.infra_delivery_failures, it)) }
            node.events_per_second?.let { Text(stringResource(R.string.infra_events_rate, number(it))) }
            node.source?.let { source ->
                Text(stringResource(R.string.infra_source, source.service, source.server), style = MachineTextStyle)
            }
            node.relay?.let { Text(stringResource(R.string.infra_relay_name, it), style = MachineTextStyle) }
        }
    }
}

@Composable
private fun spoolLabel(spool: SpoolUsage, warn: Long): String {
    val percent = if (spool.limit_bytes > 0) {
        (spool.used_bytes.toDouble() * 100 / spool.limit_bytes).toInt().coerceIn(0, 100)
    } else {
        null
    }
    return if (percent == null) {
        stringResource(R.string.infra_spool_no_limit, byteValue(spool.used_bytes), number(spool.records))
    } else {
        stringResource(R.string.infra_spool, byteValue(spool.used_bytes), byteValue(spool.limit_bytes), percent, warn, number(spool.records))
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
private fun number(value: Number): String = formatDataNumber(value, LocalConfiguration.current.locales[0])

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

private fun nodeReasonLabel(reason: String): Int = when (reason) {
    "no_heartbeat" -> R.string.infra_reason_no_heartbeat
    "relay_silent" -> R.string.infra_reason_relay_silent
    "spool" -> R.string.infra_reason_spool
    else -> R.string.infra_reason_delivery
}

private fun nodeKindLabel(kind: String): Int = when (kind) {
    "relay" -> R.string.infra_node_relay
    "host" -> R.string.infra_node_host
    else -> R.string.infra_node_agent
}

@Composable
private fun statusColor(status: String) = when (status) {
    "online" -> MaterialTheme.colorScheme.primary
    "idle", "unknown", "not_ready", "degraded" -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.error
}

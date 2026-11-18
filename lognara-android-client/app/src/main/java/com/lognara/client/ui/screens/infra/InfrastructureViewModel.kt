package com.lognara.client.ui.screens.infra

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceApiRoutes
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.InfrastructureNode
import com.lognara.client.data.InfrastructureResponse
import com.lognara.client.data.LognaraApi
import com.lognara.client.ui.screens.data.DataLoadState
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class InfrastructureNodeGroup(
    val relayName: String?,
    val relayId: Long?,
    val nodes: List<InfrastructureNode>,
)

private data class MutableNodeGroup(
    val relayName: String?,
    val relayId: Long?,
    val nodes: MutableList<InfrastructureNode> = mutableListOf(),
)

fun groupInfrastructureNodes(nodes: List<InfrastructureNode>): List<InfrastructureNodeGroup> {
    val relayNames = nodes.filter { it.kind == "relay" }.associate { it.id to it.name }
    val groups = linkedMapOf<String, MutableNodeGroup>()
    nodes.forEach { node ->
        val (key, name, relayId) = when (node.kind) {
            "relay" -> Triple("relay:${node.id}", node.name, node.id)
            "host" -> Triple("host:${node.id}", node.name, null)
            else -> {
                val id = node.relay_id
                val relayName = node.relay ?: id?.let(relayNames::get)
                if (id == null && relayName == null) {
                    Triple("unassigned", null, null)
                } else {
                    Triple("relay:${id ?: relayName}", relayName, id)
                }
            }
        }
        groups.getOrPut(key) { MutableNodeGroup(name, relayId) }.nodes += node
    }
    return groups.values
        .sortedBy { it.relayName.orEmpty() }
        .map { InfrastructureNodeGroup(it.relayName, it.relayId, it.nodes) }
}

class InfrastructureViewModel(private val api: LognaraApi, private val session: DeviceSession) : ViewModel() {
    private val _state = MutableStateFlow<DataLoadState<InfrastructureResponse>>(DataLoadState.Loading)
    val state = _state.asStateFlow()
    private var loadJob: Job? = null

    init {
        reload()
    }

    fun reload() {
        loadJob?.cancel()
        _state.value = DataLoadState.Loading
        loadJob = viewModelScope.launch {
            try {
                _state.value = DataLoadState.Content(
                    api.authenticatedJson(session, "GET", DeviceApiRoutes.INFRASTRUCTURE),
                )
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

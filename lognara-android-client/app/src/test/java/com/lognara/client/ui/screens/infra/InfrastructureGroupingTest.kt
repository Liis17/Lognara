package com.lognara.client.ui.screens.infra

import com.lognara.client.data.InfrastructureNode
import org.junit.Assert.assertEquals
import org.junit.Test

class InfrastructureGroupingTest {
    @Test
    fun groupsAgentsUnderTheirRelayAndKeepsHostStandalone() {
        val nodes = listOf(
            InfrastructureNode(kind = "agent", id = 9, name = "agent-a", relay_id = 3, relay = "relay-a", status = "online"),
            InfrastructureNode(kind = "relay", id = 3, name = "relay-a", status = "online"),
            InfrastructureNode(kind = "host", id = 4, name = "host-a", relay_id = 4, status = "online"),
        )

        val groups = groupInfrastructureNodes(nodes)

        assertEquals(2, groups.size)
        assertEquals(listOf("agent-a", "relay-a"), groups.single { it.relayName == "relay-a" }.nodes.map { it.name }.sorted())
        assertEquals("host-a", groups.single { it.relayName == "host-a" }.nodes.single().name)
    }
}

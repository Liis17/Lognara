package com.lognara.client.data

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceApiRoutesTest {
    @Test
    fun everyClientRouteIsAllowedByDockDeviceSession() {
        val repoRoot = generateSequence(File(System.getProperty("user.dir").orEmpty()).absoluteFile) { it.parentFile }
            .firstOrNull { File(it, "Lognara-dock/src/session.rs").isFile }
        checkNotNull(repoRoot) { "Could not find Lognara-dock/src/session.rs from ${System.getProperty("user.dir")}" }
        val source = File(repoRoot, "Lognara-dock/src/session.rs").readText()
        val routeFunction = source.substringAfter("fn is_device_route").substringBefore("fn issue_device_route")
        val getBlock = routeFunction.substringAfter("(method == \"GET\"").substringBefore("|| (method == \"POST\"")
        val postBlock = routeFunction.substringAfter("(method == \"POST\"").substringBefore("|| (method == \"DELETE\"")
        val deleteBlock = routeFunction.substringAfter("(method == \"DELETE\"").substringBefore("|| (method == \"GET\"")
        val issueFunction = source.substringAfter("fn issue_device_route").substringBefore("fn one_segment_after")

        DeviceApiRoutes.clientRoutes.forEach { route ->
            when {
                route.path == "/api/traces/{id}" -> assertTrue(
                    "${route.method} ${route.path} must use the trace one-segment allowlist",
                    route.method == "GET" && routeFunction.contains("one_segment_after(path, \"/api/traces/\")"),
                )
                route.path.startsWith("/api/issues/{id}/") -> {
                    val suffix = route.path.substringAfter("{id}")
                    assertTrue(
                        "${route.method} ${route.path} must use the issue action allowlist",
                        route.method == "POST" && issueFunction.contains("(\"$suffix\", \"POST\")"),
                    )
                }
                else -> {
                    val block = when (route.method) {
                        "GET" -> getBlock
                        "POST" -> postBlock
                        "DELETE" -> deleteBlock
                        else -> error("Unexpected client method ${route.method}")
                    }
                    assertTrue("${route.method} ${route.path} is missing from session.rs", pathLiterals(block).contains(route.path))
                }
            }
        }
    }

    @Test
    fun dynamicRouteBuildersEncodeIdentifiersAsSingleSegments() {
        assertEquals("/api/issues/f1_a%2Fb/details", DeviceApiRoutes.issueDetails("f1_a/b"))
        assertTrue(DeviceApiRoutes.trace("trace/a", "from", "to").startsWith("/api/traces/trace%2Fa?"))
    }

    private fun pathLiterals(block: String): Set<String> =
        Regex("\"(/api/[^\"\\s]+)\"").findAll(block).map { it.groupValues[1] }.toSet()
}

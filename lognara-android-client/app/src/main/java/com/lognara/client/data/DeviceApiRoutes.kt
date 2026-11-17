package com.lognara.client.data

import okhttp3.HttpUrl.Companion.toHttpUrl

data class DeviceApiRoute(val method: String, val path: String)

object DeviceApiRoutes {
    val clientRoutes = setOf(
        DeviceApiRoute("GET", "/api/me"),
        DeviceApiRoute("GET", "/api/logs/stream"),
        DeviceApiRoute("GET", "/api/infrastructure"),
        DeviceApiRoute("GET", "/api/series"),
        DeviceApiRoute("GET", "/api/series/favorites"),
        DeviceApiRoute("GET", "/api/series/sources"),
        DeviceApiRoute("GET", "/api/traces/{id}"),
        DeviceApiRoute("POST", "/api/logs/search"),
        DeviceApiRoute("POST", "/api/stats/summary"),
        DeviceApiRoute("POST", "/api/stats/group-by"),
        DeviceApiRoute("POST", "/api/issues/search"),
        DeviceApiRoute("POST", "/api/issues/{id}/details"),
        DeviceApiRoute("POST", "/api/issues/{id}/resolve"),
        DeviceApiRoute("POST", "/api/issues/{id}/reopen"),
        DeviceApiRoute("POST", "/api/series/lookup"),
        DeviceApiRoute("POST", "/api/series/query"),
        DeviceApiRoute("DELETE", "/api/devices/current"),
    )

    const val ME = "/api/me"
    const val LIVE = "/api/logs/stream"
    const val INFRASTRUCTURE = "/api/infrastructure"
    const val SERIES = "/api/series"
    const val FAVORITES = "/api/series/favorites"
    const val SERIES_SOURCES = "/api/series/sources"
    const val LOG_SEARCH = "/api/logs/search"
    const val SUMMARY = "/api/stats/summary"
    const val GROUP_BY = "/api/stats/group-by"
    const val ISSUE_SEARCH = "/api/issues/search"
    const val SERIES_LOOKUP = "/api/series/lookup"
    const val SERIES_QUERY = "/api/series/query"
    const val DISCONNECT = "/api/devices/current"

    fun trace(traceId: String, from: String, to: String, cursor: String? = null): String =
        routeUrl()
            .addPathSegment("api")
            .addPathSegment("traces")
            .addPathSegment(traceId.lowercase())
            .addQueryParameter("from", from)
            .addQueryParameter("to", to)
            .addQueryParameter("limit", "50")
            .apply { if (cursor != null) addQueryParameter("cursor", cursor) }
            .build()
            .encodedPathAndQuery()

    fun issueDetails(fingerprint: String): String = issuePath(fingerprint, "details")
    fun issueResolve(fingerprint: String): String = issuePath(fingerprint, "resolve")
    fun issueReopen(fingerprint: String): String = issuePath(fingerprint, "reopen")

    fun seriesList(server: String, service: String): String = routeUrl()
        .addPathSegment("api")
        .addPathSegment("series")
        .addQueryParameter("group_by", "server")
        .addQueryParameter("server", server)
        .addQueryParameter("service", service)
        .build()
        .encodedPathAndQuery()

    private fun issuePath(fingerprint: String, action: String): String = routeUrl()
        .addPathSegment("api")
        .addPathSegment("issues")
        .addPathSegment(fingerprint)
        .addPathSegment(action)
        .build()
        .encodedPath

    private fun routeUrl() = "https://device-route.invalid".toHttpUrl().newBuilder()

    private fun okhttp3.HttpUrl.encodedPathAndQuery(): String = encodedPath + "?" + encodedQuery
}

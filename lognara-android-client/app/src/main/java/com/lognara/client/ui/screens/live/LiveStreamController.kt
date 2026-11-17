package com.lognara.client.ui.screens.live

import com.lognara.client.data.ApiException
import com.lognara.client.data.LiveConnection
import com.lognara.client.data.LiveStreamListener
import java.util.concurrent.CancellationException
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

enum class LiveStatus { Disconnected, Connecting, Connected, Reconnecting, CoreError }

class LiveStreamController(
    private val scope: CoroutineScope,
    private val openStream: (LiveStreamListener) -> LiveConnection,
    private val checkSession: suspend () -> Unit,
    private val onStatus: (LiveStatus) -> Unit,
    private val onEvent: (String?, String) -> Unit,
    private val waitBeforeRetry: suspend (Long) -> Unit = { delay(it) },
) {
    private var task: Job? = null
    private var connection: LiveConnection? = null

    fun start() {
        if (task?.isActive == true) return
        task = scope.launch { reconnectLoop() }
    }

    fun stop() {
        connection?.cancel()
        connection = null
        task?.cancel()
        task = null
        onStatus(LiveStatus.Disconnected)
    }

    private suspend fun reconnectLoop() {
        var backoffMs = 1_000L
        try {
            while (currentCoroutineContext().isActive) {
                currentCoroutineContext().ensureActive()
                onStatus(LiveStatus.Connecting)
                val ended = CompletableDeferred<Int?>()
                val retryDelay = AtomicLong(backoffMs)
                var openedConnection: LiveConnection? = null
                val endCode = try {
                    openedConnection = openStream(
                        LiveStreamListener(
                            onOpen = {
                                retryDelay.set(1_000L)
                                onStatus(LiveStatus.Connected)
                            },
                            onEvent = onEvent,
                            onClosed = { ended.complete(null) },
                            onFailure = { status -> ended.complete(status) },
                        ),
                    )
                    connection = openedConnection
                    ended.await()
                } catch (exception: CancellationException) {
                    throw exception
                } catch (_: Exception) {
                    null
                } finally {
                    openedConnection?.cancel()
                    if (connection === openedConnection) connection = null
                }

                if (endCode == 401) break
                try {
                    checkSession()
                } catch (exception: CancellationException) {
                    throw exception
                } catch (_: ApiException.Unauthorized) {
                    break
                } catch (_: Exception) {
                    // Любой ответ кроме 401 разрешает повторить подключение.
                }
                if (!currentCoroutineContext().isActive) break
                backoffMs = retryDelay.get()
                onStatus(LiveStatus.Reconnecting)
                waitBeforeRetry(backoffMs)
                backoffMs = (backoffMs * 2).coerceAtMost(30_000L)
            }
        } finally {
            connection?.cancel()
            connection = null
            onStatus(LiveStatus.Disconnected)
        }
    }
}

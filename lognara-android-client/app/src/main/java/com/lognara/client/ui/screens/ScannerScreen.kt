package com.lognara.client.ui.screens

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.SystemClock
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BinaryBitmap
import com.google.zxing.LuminanceSource
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.common.HybridBinarizer
import com.lognara.client.R
import com.lognara.client.ui.PairingUiState
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun ScannerScreen(
    pairing: PairingUiState,
    onBack: () -> Unit,
    onPayload: (String) -> Unit,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var hasPermission by remember {
        mutableStateOf(context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var requestedPermission by remember { mutableStateOf(false) }
    val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        requestedPermission = true
        hasPermission = granted
    }

    DisposableEffect(lifecycleOwner, context) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                hasPermission = context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(Unit) {
        if (!hasPermission) {
            requestedPermission = true
            permissionLauncher.launch(Manifest.permission.CAMERA)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.scanner_title)) },
                navigationIcon = { BackButton(onClick = onBack) },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { insets ->
        Column(
            modifier = Modifier.fillMaxSize().padding(insets).padding(horizontal = 24.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(stringResource(R.string.scanner_instructions), style = MaterialTheme.typography.bodyLarge)
            if (hasPermission) {
                CameraQrPreview(
                    pairingError = pairing.error,
                    isConnecting = pairing.isConnecting,
                    onPayload = onPayload,
                    modifier = Modifier.fillMaxWidth().weight(1f),
                )
                Text(stringResource(R.string.camera_scanning), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                Spacer(Modifier.weight(1f))
                Text(
                    stringResource(if (requestedPermission) R.string.camera_permission_denied else R.string.camera_permission_request),
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Button(
                    onClick = {
                        if (requestedPermission) {
                            context.startActivity(
                                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                                    data = Uri.fromParts("package", context.packageName, null)
                                },
                            )
                        } else {
                            permissionLauncher.launch(Manifest.permission.CAMERA)
                        }
                    },
                    modifier = Modifier.fillMaxWidth(),
                    shape = MaterialTheme.shapes.medium,
                ) {
                    Text(stringResource(if (requestedPermission) R.string.open_app_settings else R.string.camera_permission_request))
                }
                Spacer(Modifier.weight(1f))
            }
            pairing.error?.let { ConnectionErrorMessage(it) }
            if (pairing.isConnecting) {
                PairButton(pairing, enabled = false, onClick = {})
            }
        }
    }
}

@Composable
private fun CameraQrPreview(
    pairingError: com.lognara.client.ui.ConnectionError?,
    isConnecting: Boolean,
    onPayload: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val previewDescription = stringResource(R.string.camera_preview_content_description)
    val viewfinderDescription = stringResource(R.string.qr_viewfinder_content_description)
    val currentOnPayload by rememberUpdatedState(onPayload)
    var previewView by remember { mutableStateOf<PreviewView?>(null) }
    var cameraUnavailable by remember { mutableStateOf(false) }
    val payloadHandled = remember { AtomicBoolean(false) }
    val currentlyConnecting = remember { AtomicBoolean(false) }
    val lastAnalyzedAt = remember { AtomicLong(0L) }
    SideEffect { currentlyConnecting.set(isConnecting) }
    LaunchedEffect(pairingError, isConnecting) {
        if (pairingError != null && !isConnecting) payloadHandled.set(false)
    }

    DisposableEffect(previewView, lifecycleOwner) {
        val view = previewView
        if (view == null) return@DisposableEffect onDispose { }

        val executor = Executors.newSingleThreadExecutor()
        var provider: ProcessCameraProvider? = null
        var disposed = false
        val providerFuture = ProcessCameraProvider.getInstance(context)
        providerFuture.addListener({
            if (disposed) return@addListener
            try {
                provider = providerFuture.get()
                val preview = Preview.Builder().build().also { it.surfaceProvider = view.surfaceProvider }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(executor) { image ->
                    try {
                        val now = SystemClock.elapsedRealtime()
                        if (!currentlyConnecting.get() && now - lastAnalyzedAt.get() >= 180L) {
                            lastAnalyzedAt.set(now)
                            val decoded = decodeQrFromYuv(image)
                            if (decoded != null && payloadHandled.compareAndSet(false, true)) {
                                view.post { currentOnPayload(decoded) }
                            }
                        }
                    } catch (_: Exception) {
                        // Ignore malformed frames and continue analyzing the next one.
                    } finally {
                        image.close()
                    }
                }
                provider?.unbindAll()
                provider?.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                cameraUnavailable = false
            } catch (_: Exception) {
                cameraUnavailable = true
            }
        }, context.mainExecutor)

        onDispose {
            disposed = true
            provider?.unbindAll()
            executor.shutdown()
        }
    }

    Box(modifier = modifier, contentAlignment = Alignment.Center) {
        AndroidView(
            factory = { PreviewView(it).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } },
            modifier = Modifier.fillMaxSize().semantics {
                contentDescription = previewDescription
            },
            update = { previewView = it },
        )
        Canvas(
            modifier = Modifier.size(260.dp).semantics {
                contentDescription = viewfinderDescription
            },
        ) {
            val length = size.minDimension * 0.18f
            val stroke = 4.dp.toPx()
            val inset = stroke / 2
            val right = size.width - inset
            val bottom = size.height - inset
            val path = Path().apply {
                moveTo(inset, length); lineTo(inset, inset); lineTo(length, inset)
                moveTo(right - length, inset); lineTo(right, inset); lineTo(right, length)
                moveTo(right, bottom - length); lineTo(right, bottom); lineTo(right - length, bottom)
                moveTo(length, bottom); lineTo(inset, bottom); lineTo(inset, bottom - length)
            }
            drawPath(path, Color(0xFFC07A52), style = Stroke(width = stroke, cap = StrokeCap.Round))
        }
        if (cameraUnavailable) {
            Text(
                text = stringResource(R.string.camera_unavailable),
                modifier = Modifier.align(Alignment.BottomCenter).padding(20.dp),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

private fun decodeQrFromYuv(image: ImageProxy): String? {
    return try {
        val plane = image.planes.firstOrNull() ?: return null
        val width = image.width
        val height = image.height
        val rowStride = plane.rowStride
        val pixelStride = plane.pixelStride
        val buffer = plane.buffer.duplicate()
        val bufferStart = buffer.position()
        val luminance = ByteArray(width * height)
        for (row in 0 until height) {
            for (column in 0 until width) {
                luminance[row * width + column] = buffer.get(bufferStart + row * rowStride + column * pixelStride)
            }
        }
        val rotated = rotateLuminance(luminance, width, height, image.imageInfo.rotationDegrees)
        val source: LuminanceSource = PlanarYUVLuminanceSource(
            rotated.pixels,
            rotated.width,
            rotated.height,
            0,
            0,
            rotated.width,
            rotated.height,
            false,
        )
        decodeQrFromSource(source)
    } catch (_: Exception) {
        null
    }
}

internal fun decodeQrFromSource(source: LuminanceSource): String? = try {
    QRCodeReader().decode(BinaryBitmap(HybridBinarizer(source))).text
} catch (_: Exception) {
    null
}

internal data class RotatedLuminance(val pixels: ByteArray, val width: Int, val height: Int)

internal fun rotateLuminance(pixels: ByteArray, width: Int, height: Int, rotationDegrees: Int): RotatedLuminance {
    val rotation = ((rotationDegrees % 360) + 360) % 360
    if (rotation != 90 && rotation != 180 && rotation != 270) {
        return RotatedLuminance(pixels, width, height)
    }
    val rotatedWidth = if (rotation == 180) width else height
    val rotatedHeight = if (rotation == 180) height else width
    val rotated = ByteArray(pixels.size)
    for (y in 0 until height) {
        for (x in 0 until width) {
            val (rotatedX, rotatedY) = when (rotation) {
                90 -> height - 1 - y to x
                180 -> width - 1 - x to height - 1 - y
                else -> y to width - 1 - x
            }
            rotated[rotatedY * rotatedWidth + rotatedX] = pixels[y * width + x]
        }
    }
    return RotatedLuminance(rotated, rotatedWidth, rotatedHeight)
}

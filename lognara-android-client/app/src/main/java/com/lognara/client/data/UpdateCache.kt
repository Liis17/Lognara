package com.lognara.client.data

import java.io.File

internal fun clearUpdateCache(directory: File) {
    directory.listFiles()?.forEach { it.deleteRecursively() }
}

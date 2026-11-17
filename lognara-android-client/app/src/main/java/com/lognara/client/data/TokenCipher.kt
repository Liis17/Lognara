package com.lognara.client.data

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.security.UnrecoverableKeyException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

data class EncryptedToken(val iv: ByteArray, val ciphertext: ByteArray) {
    override fun toString(): String = "EncryptedToken(iv=***, ciphertext=***)"
}

interface TokenCipher {
    fun encrypt(token: String, serverUrl: String): EncryptedToken
    fun decrypt(encryptedToken: EncryptedToken, serverUrl: String): String
    fun clear()
}

class AndroidKeystoreTokenCipher : TokenCipher {
    override fun encrypt(token: String, serverUrl: String): EncryptedToken {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, encryptionKey())
        cipher.updateAAD(serverUrl.toByteArray(Charsets.UTF_8))
        val encrypted = cipher.doFinal(token.toByteArray(Charsets.UTF_8))
        return EncryptedToken(cipher.iv, encrypted)
    }

    override fun decrypt(encryptedToken: EncryptedToken, serverUrl: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, decryptionKey(), GCMParameterSpec(TAG_BITS, encryptedToken.iv))
        cipher.updateAAD(serverUrl.toByteArray(Charsets.UTF_8))
        return cipher.doFinal(encryptedToken.ciphertext).toString(Charsets.UTF_8)
    }

    override fun clear() {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        if (keyStore.containsAlias(KEY_ALIAS)) keyStore.deleteEntry(KEY_ALIAS)
    }

    private fun encryptionKey(): SecretKey = existingKey() ?: generateKey()

    private fun decryptionKey(): SecretKey = existingKey()
        ?: throw UnrecoverableKeyException("Lognara token key is missing")

    private fun existingKey(): SecretKey? {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        return keyStore.getKey(KEY_ALIAS, null) as? SecretKey
    }

    private fun generateKey(): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val KEY_ALIAS = "lognara_device_token"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val TAG_BITS = 128
    }
}

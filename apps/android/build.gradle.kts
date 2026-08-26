// Versioned plugins keep local and CI builds reproducible.
plugins {
    id("com.android.application") version "8.11.1" apply false // Android application toolchain.
    id("org.jetbrains.kotlin.android") version "2.2.0" apply false // Kotlin Android compiler.
}

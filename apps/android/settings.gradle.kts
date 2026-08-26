// The Android capture client remains an independent build so desktop and server work do not require the SDK.
pluginManagement {
    repositories {
        google() // Android Gradle Plugin releases are published here.
        mavenCentral() // Kotlin and supporting JVM plugins are published here.
        gradlePluginPortal() // Gradle resolves plugin marker artifacts here.
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS) // One repository policy prevents drift.
    repositories {
        google() // AndroidX artifacts are published here.
        mavenCentral() // OkHttp and Kotlin dependencies are published here.
    }
}

rootProject.name = "AialraCapture" // The name appears in Gradle diagnostics and Android Studio.
include(":app") // The bootstrap contains one phone application module.

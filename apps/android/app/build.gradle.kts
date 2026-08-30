plugins {
    id("com.android.application") // Produces the installable capture application.
    id("org.jetbrains.kotlin.android") // Compiles the foreground recorder written in Kotlin.
}

android {
    namespace = "online.aialra.capture" // Resource and generated-code namespace.
    compileSdk = 36 // The installed Android 16 SDK validates current foreground-service rules.

    defaultConfig {
        applicationId = "online.aialra.capture" // Stable package identity for local device installs.
        minSdk = 26 // Notification channels and modern background limits start at this baseline.
        targetSdk = 36 // Runtime behavior follows the current platform contract.
        versionCode = 1 // First internal bootstrap build.
        versionName = "0.1.0" // Human-readable bootstrap version.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner" // Reserved for device tests.
        val pairingServer = providers.environmentVariable("AIALRA_ANDROID_PAIRING_SERVER").orElse("").get()
        buildConfigField("String", "PAIRING_SERVER_URL", "\"${pairingServer.replace("\\", "\\\\").replace("\"", "\\\"")}\"")
    }

    buildFeatures { buildConfig = true }

    buildTypes {
        release {
            isMinifyEnabled = false // Debuggable bootstrap preserves readable failure traces.
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"), // Android baseline rules.
                "proguard-rules.pro", // Project-specific rules remain explicit.
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17 // Android Gradle Plugin supported bytecode level.
        targetCompatibility = JavaVersion.VERSION_17 // Device bytecode matches the compiler source level.
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) // Match the Java bytecode level.
    }
}

dependencies {
    implementation("androidx.activity:activity-ktx:1.10.1") // Permission and lifecycle helpers.
    implementation("androidx.core:core-ktx:1.16.0") // Foreground service compatibility helpers.
    implementation("com.squareup.okhttp3:okhttp:4.12.0") // Reliable WebSocket transport and binary frames.
    testImplementation("junit:junit:4.13.2") // Pure JVM transport contract tests.
}

/*
 * Android library for the React Native binding (Phase 10).
 *
 * UNVERIFIED: not built — no Android SDK in this environment.
 * The consuming app must:
 *  - include the Kotlin binding as a Gradle module named
 *    `:codec-what-kotlin` (see bindings/kotlin/README.md for its
 *    native-library build), or adjust the `project(...)` reference
 *    below to the module name the app uses;
 *  - register `dev.codecwhat.CodecWhatPackage` in its
 *    `ReactNativeHost.getPackages()` (or the New Architecture
 *    equivalent).
 */
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.codecwhat"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation("com.facebook.react:react-android")
    // The Kotlin binding provides the JNI layer over the C ABI.
    implementation(project(":codec-what-kotlin"))
}

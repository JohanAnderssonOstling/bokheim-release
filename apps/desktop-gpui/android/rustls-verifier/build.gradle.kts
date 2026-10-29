plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android") version "2.1.20"
}

android {
    namespace = "org.rustls.platformverifier"
    compileSdk = 36
    defaultConfig {
        minSdk = 26
        buildConfigField("boolean", "TEST", "false")
    }
    buildFeatures.buildConfig = true
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

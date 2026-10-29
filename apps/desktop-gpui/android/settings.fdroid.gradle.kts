pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

check(providers.gradleProperty("bokheim.fdroid").orNull == "true") {
    "This checkout was prepared for F-Droid; use -Pbokheim.fdroid=true"
}
rootProject.name = "BokheimGpui"
include(":rustls-verifier")

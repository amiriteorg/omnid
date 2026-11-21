pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // GeckoView, and nothing else, from Mozilla.
        exclusiveContent {
            forRepository { maven("https://maven.mozilla.org/maven2/") }
            filter { includeGroup("org.mozilla.geckoview") }
        }
    }
}

rootProject.name = "omnid"
include(":app")

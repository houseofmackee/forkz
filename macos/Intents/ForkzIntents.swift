// forkz's actions in Shortcuts (App Intents). Each action hands a forkz://
// link to the app (see src/links.rs), which does the work; the lists of
// sites, synclets, smart folders, workspaces, and commands come from the
// app too. Built into the app by build.rs; scripts/bundle.sh adds the
// metadata Shortcuts reads.

import AppIntents
import Foundation

// The app's side, in Rust (src/ui/intents.rs).
@_silgen_name("forkz_intent_link")
func forkzIntentLink(_ link: UnsafePointer<CChar>)

@_silgen_name("forkz_intent_names")
func forkzIntentNames(_ kind: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?

@_silgen_name("forkz_intent_free")
func forkzIntentFree(_ text: UnsafeMutablePointer<CChar>?)

/// (id, name) pairs: "site", "synclet", "smart-folder", "workspace", or "command".
func forkzNames(_ kind: String) -> [(id: String, name: String)] {
    guard let raw = kind.withCString({ forkzIntentNames($0) }) else { return [] }
    defer { forkzIntentFree(raw) }
    return String(cString: raw).split(separator: "\n").compactMap { line in
        let parts = line.split(separator: "\t", maxSplits: 1).map(String.init)
        return parts.count == 2 ? (id: parts[0], name: parts[1]) : nil
    }
}

/// Hands a forkz:// link to the app.
func follow(_ action: String, _ values: [(String, String?)]) {
    var allowed = CharacterSet.alphanumerics
    allowed.insert(charactersIn: "-._~/")
    let query = values.compactMap { key, value -> String? in
        guard let value, let encoded = value.addingPercentEncoding(withAllowedCharacters: allowed) else { return nil }
        return "\(key)=\(encoded)"
    }.joined(separator: "&")
    "forkz://\(action)?\(query)".withCString { forkzIntentLink($0) }
}

struct ForkzError: Error, CustomLocalizedStringResourceConvertible {
    let localizedStringResource: LocalizedStringResource
}

// Choices.

enum PaneChoice: String, AppEnum {
    case active, other, left, right

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Pane"
    static let caseDisplayRepresentations: [PaneChoice: DisplayRepresentation] = [
        .active: "Active Pane",
        .other: "Other Pane",
        .left: "Left Pane",
        .right: "Right Pane",
    ]
}

enum KindChoice: String, AppEnum {
    case any, folders, files, images, movies, audio, documents, archives, code

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Kind"
    static let caseDisplayRepresentations: [KindChoice: DisplayRepresentation] = [
        .any: "Any Kind",
        .folders: "Folders",
        .files: "Files",
        .images: "Images",
        .movies: "Movies",
        .audio: "Music and Audio",
        .documents: "Documents",
        .archives: "Archives",
        .code: "Source Code",
    ]
}

// Things forkz has saved, to pick from.

struct SiteEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "forkz Site"
    static let defaultQuery = SiteQuery()
    let id: String
    let name: String
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(name)") }
}

struct SiteQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [SiteEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [SiteEntity] {
        forkzNames("site").map { SiteEntity(id: $0.id, name: $0.name) }
    }
}

struct SyncletEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "forkz Synclet"
    static let defaultQuery = SyncletQuery()
    let id: String
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(id)") }
}

struct SyncletQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [SyncletEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [SyncletEntity] {
        forkzNames("synclet").map { SyncletEntity(id: $0.id) }
    }
}

struct SmartFolderEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "forkz Smart Folder"
    static let defaultQuery = SmartFolderQuery()
    let id: String
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(id)") }
}

struct SmartFolderQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [SmartFolderEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [SmartFolderEntity] {
        forkzNames("smart-folder").map { SmartFolderEntity(id: $0.id) }
    }
}

struct WorkspaceEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "forkz Workspace"
    static let defaultQuery = WorkspaceQuery()
    let id: String
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(id)") }
}

struct WorkspaceQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [WorkspaceEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [WorkspaceEntity] {
        forkzNames("workspace").map { WorkspaceEntity(id: $0.id) }
    }
}

struct CommandEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "forkz Command"
    static let defaultQuery = CommandQuery()
    let id: String
    let name: String
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(name)") }
}

struct CommandQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [CommandEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [CommandEntity] {
        forkzNames("command").map { CommandEntity(id: $0.id, name: $0.name) }
    }
}

// The actions.

struct OpenFolderIntent: AppIntent {
    static let title: LocalizedStringResource = "Open in forkz"
    static let description = IntentDescription(
        "Shows a folder in a forkz pane, or a file selected in its folder.")
    static let openAppWhenRun = true

    @Parameter(title: "Folder or File")
    var item: IntentFile

    @Parameter(title: "Pane", default: .active)
    var pane: PaneChoice

    @Parameter(title: "In a New Tab", default: false)
    var newTab: Bool

    static var parameterSummary: some ParameterSummary {
        Summary("Open \(\.$item) in the \(\.$pane)") { \.$newTab }
    }

    func perform() async throws -> some IntentResult {
        guard let url = item.fileURL else {
            throw ForkzError(localizedStringResource: "Choose a folder or file on this Mac.")
        }
        follow("open", [("path", url.path), ("pane", pane.rawValue), ("tab", newTab ? "new" : nil)])
        return .result()
    }
}

struct ConnectToSiteIntent: AppIntent {
    static let title: LocalizedStringResource = "Connect to Site in forkz"
    static let description = IntentDescription("Connects to a saved site in a forkz pane.")
    static let openAppWhenRun = true

    @Parameter(title: "Site")
    var site: SiteEntity

    @Parameter(title: "Folder on the Server", description: "A path such as /var/www (empty: the site's own).")
    var path: String?

    @Parameter(title: "Pane", default: .active)
    var pane: PaneChoice

    static var parameterSummary: some ParameterSummary {
        Summary("Connect to \(\.$site) in the \(\.$pane)") { \.$path }
    }

    func perform() async throws -> some IntentResult {
        follow("connect", [("site", site.id), ("path", path), ("pane", pane.rawValue)])
        return .result()
    }
}

struct SearchIntent: AppIntent {
    static let title: LocalizedStringResource = "Search in forkz"
    static let description = IntentDescription(
        "Searches a folder on this Mac and everything in it, and shows what is found in a forkz pane.")
    static let openAppWhenRun = true

    @Parameter(title: "Folder")
    var folder: IntentFile

    @Parameter(title: "Text", description: "Part of a name, or a pattern such as *.pdf.")
    var text: String?

    @Parameter(title: "Kind", default: .any)
    var kind: KindChoice

    @Parameter(title: "Search Contents of Files", default: false)
    var contents: Bool

    @Parameter(title: "Pane", default: .active)
    var pane: PaneChoice

    static var parameterSummary: some ParameterSummary {
        Summary("Search \(\.$folder) for \(\.$text)") {
            \.$kind
            \.$contents
            \.$pane
        }
    }

    func perform() async throws -> some IntentResult {
        guard let url = folder.fileURL else {
            throw ForkzError(localizedStringResource: "Choose a folder on this Mac.")
        }
        follow("search", [
            ("path", url.path), ("text", text), ("kind", kind == .any ? nil : kind.rawValue),
            ("contents", contents ? "1" : nil), ("pane", pane.rawValue),
        ])
        return .result()
    }
}

struct RunSyncletIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Synclet in forkz"
    static let description = IntentDescription(
        "Opens a saved sync in forkz and compares its folders (nothing is copied until you choose to).")
    static let openAppWhenRun = true

    @Parameter(title: "Synclet")
    var synclet: SyncletEntity

    static var parameterSummary: some ParameterSummary { Summary("Open \(\.$synclet)") }

    func perform() async throws -> some IntentResult {
        follow("synclet", [("name", synclet.id)])
        return .result()
    }
}

struct OpenSmartFolderIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Smart Folder in forkz"
    static let description = IntentDescription("Runs a saved search and shows what it finds in a forkz pane.")
    static let openAppWhenRun = true

    @Parameter(title: "Smart Folder")
    var smartFolder: SmartFolderEntity

    @Parameter(title: "Pane", default: .active)
    var pane: PaneChoice

    static var parameterSummary: some ParameterSummary { Summary("Open \(\.$smartFolder) in the \(\.$pane)") }

    func perform() async throws -> some IntentResult {
        follow("smart-folder", [("name", smartFolder.id), ("pane", pane.rawValue)])
        return .result()
    }
}

struct OpenWorkspaceIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Workspace in forkz"
    static let description = IntentDescription("Opens a saved set of tabs in both forkz panes.")
    static let openAppWhenRun = true

    @Parameter(title: "Workspace")
    var workspace: WorkspaceEntity

    static var parameterSummary: some ParameterSummary { Summary("Open \(\.$workspace)") }

    func perform() async throws -> some IntentResult {
        follow("workspace", [("name", workspace.id)])
        return .result()
    }
}

struct RunCommandIntent: AppIntent {
    static let title: LocalizedStringResource = "Run forkz Command"
    static let description = IntentDescription(
        "Runs a forkz menu command on the active pane, such as Compare Panes or Find Duplicates. Commands that delete or move items are not offered.")
    static let openAppWhenRun = true

    @Parameter(title: "Command")
    var command: CommandEntity

    static var parameterSummary: some ParameterSummary { Summary("Run \(\.$command)") }

    func perform() async throws -> some IntentResult {
        follow("command", [("name", command.id)])
        return .result()
    }
}

struct ForkzShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: OpenFolderIntent(),
            phrases: ["Open a folder in \(.applicationName)"],
            shortTitle: "Open Folder",
            systemImageName: "folder")
        AppShortcut(
            intent: ConnectToSiteIntent(),
            phrases: ["Connect to a site in \(.applicationName)"],
            shortTitle: "Connect to Site",
            systemImageName: "server.rack")
        AppShortcut(
            intent: SearchIntent(),
            phrases: ["Search in \(.applicationName)"],
            shortTitle: "Search",
            systemImageName: "magnifyingglass")
    }
}

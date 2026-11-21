import XCTest

final class AlwaysOnHostTests: XCTestCase {
    @MainActor
    func testSendInCodexHistoryWithComposerFocused() {
        continueAfterFailure = false
        let app = XCUIApplication()
        launch(app, mode: "codex-history", run: UUID().uuidString)
        let row = app.buttons["sessionRow-mac-codex-history"]
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.tap()
        XCTAssertTrue(app.navigationBars["Laptop Codex history"].waitForExistence(timeout: 10))

        let composer = app.descendants(matching: .any)["messageComposer"].firstMatch
        let send = app.buttons["sendMessageButton"]
        let prompt = "Run the remaining workspace check and summarize the result without changing any files. Confirm that the expected build files are present and report the project directory. Keep your response brief and don't change any source files or settings."
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "No files were changed.")).firstMatch.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText(prompt)
        XCTAssertTrue(send.isEnabled)
        send.tap()

        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label == %@", prompt)).firstMatch.waitForExistence(timeout: 10))
        let reply = app.staticTexts.containing(NSPredicate(format: "label BEGINSWITH %@", "Workspace check complete.")).firstMatch
        XCTAssertTrue(reply.wait(for: \.isHittable, toEqual: true, timeout: 10))
        XCTAssertTrue(app.navigationBars["Laptop Codex history"].buttons["Latest message"].isHittable)
    }

    @MainActor
    func testLinuxFallbackCreatesBothAgentsWithoutMovingSavedLaptopSession() {
        continueAfterFailure = false
        let app = XCUIApplication()
        let run = UUID().uuidString
        launch(app, mode: "online", run: run)
        XCTAssertTrue(app.staticTexts["Saved laptop session"].waitForExistence(timeout: 10))
        app.terminate()

        launch(app, mode: "offline", run: run)
        app.tabBars.buttons["Devices"].tap()
        XCTAssertTrue(app.staticTexts["Mac"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Linux"].exists)
        app.tabBars.buttons["Sessions"].tap()
        let picker = app.buttons["sessionDevicePicker"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        XCTAssertTrue(picker.label.contains("Test host (Linux)"))
        XCTAssertFalse(app.staticTexts["Saved laptop session"].exists)
        picker.tap()
        app.buttons["Test laptop (Mac)"].tap()

        let saved = app.buttons["sessionRow-mac-saved"]
        XCTAssertTrue(saved.waitForExistence(timeout: 10))
        XCTAssertTrue(saved.label.contains("Offline"))
        saved.tap()
        XCTAssertTrue(app.staticTexts["This device is offline. Reconnect it to continue this session."].exists)
        XCTAssertFalse(app.buttons["sendMessageButton"].isEnabled)
        let stop = app.buttons["stopTurnButton"]
        XCTAssertTrue(stop.exists)
        XCTAssertFalse(stop.isEnabled)
        app.navigationBars.buttons["BackButton"].tap()

        let project = "/srv/project"
        for (agent, title, prefilled) in [("Claude Code", "Linux Claude Code session", false), ("Codex", "Linux Codex session", true)] {
            app.buttons["newSessionButton"].tap()
            XCTAssertTrue(app.staticTexts["Starting on Test host (Linux)"].waitForExistence(timeout: 10))
            app.buttons[agent].tap()
            let directory = app.textFields["newSessionDirectory"]
            XCTAssertTrue(directory.exists)
            if !prefilled {
                directory.tap()
                directory.typeText(project)
            }
            XCTAssertEqual(directory.value as? String, project)
            app.buttons["startSessionButton"].tap()
            XCTAssertTrue(app.navigationBars[title].waitForExistence(timeout: 10))
            app.navigationBars.buttons["BackButton"].tap()
            XCTAssertTrue(saved.waitForExistence(timeout: 10))
            XCTAssertTrue(saved.label.contains("Offline"))
        }
    }

    @MainActor
    func testCreatedLaptopSessionRemainsVisibleAfterGoingOfflineInConversation() {
        continueAfterFailure = false
        let app = XCUIApplication()
        let run = UUID().uuidString
        launch(app, mode: "online", run: run)
        XCTAssertTrue(app.staticTexts["Saved laptop session"].waitForExistence(timeout: 10))
        app.buttons["newSessionButton"].tap()
        app.buttons["Codex"].tap()
        let directory = app.textFields["newSessionDirectory"]
        XCTAssertTrue(directory.exists)
        XCTAssertEqual(directory.value as? String, "/Users/test/repo")
        app.buttons["startSessionButton"].tap()
        XCTAssertTrue(app.navigationBars["New laptop Codex session"].waitForExistence(timeout: 10))
        app.terminate()

        launch(app, mode: "offline", run: run)
        let picker = app.buttons["sessionDevicePicker"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        picker.tap()
        app.buttons["Test laptop (Mac)"].tap()
        let created = app.buttons["sessionRow-codex-mac"]
        XCTAssertTrue(created.waitForExistence(timeout: 10))
        XCTAssertTrue(created.label.contains("Offline"))
        XCTAssertTrue(app.buttons["sessionRow-mac-saved"].exists)
    }

    @MainActor
    func testNewSessionIsDisabledWhenEveryDeviceIsOffline() {
        let app = XCUIApplication()
        launch(app, mode: "all-offline", run: UUID().uuidString)
        XCTAssertTrue(app.staticTexts["Connect a device to start a new session."].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["newSessionButton"].isEnabled)
    }

    @MainActor
    private func launch(_ app: XCUIApplication, mode: String, run: String) {
        app.launchEnvironment = ["REMOTE_UI_TEST_MODE": mode, "REMOTE_UI_TEST_RUN": run]
        app.launch()
    }
}

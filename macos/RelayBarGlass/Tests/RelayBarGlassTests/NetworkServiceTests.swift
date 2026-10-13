import XCTest
@testable import RelayBarGlass

final class NetworkServiceTests: XCTestCase {
    private let wiredOrder = """
    An asterisk (*) denotes that a network service is disabled.
    (1) Ethernet
    (Hardware Port: Ethernet, Device: en0)

    (2) Thunderbolt Bridge
    (Hardware Port: Thunderbolt Bridge, Device: bridge0)

    """

    private let laptopOrder = """
    An asterisk (*) denotes that a network service is disabled.
    (1) USB 10/100/1000 LAN
    (Hardware Port: USB 10/100/1000 LAN, Device: en6)

    (2) Wi-Fi
    (Hardware Port: Wi-Fi, Device: en0)

    """

    func testReadsTheDefaultRouteDevice() {
        let route = "   route to: default\ndestination: default\n  interface: en0\n      flags: <UP,GATEWAY>\n"
        XCTAssertEqual(AppModel.defaultRouteDevice(from: route), "en0")
        XCTAssertNil(AppModel.defaultRouteDevice(from: nil))
        XCTAssertNil(AppModel.defaultRouteDevice(from: "route: writing to routing socket: not in table\n"))
    }

    func testPicksTheServiceThatOwnsTheDefaultRoute() {
        XCTAssertEqual(AppModel.networkService(forDevice: "en0", serviceOrder: wiredOrder), "Ethernet")
        XCTAssertEqual(AppModel.networkService(forDevice: "en0", serviceOrder: laptopOrder), "Wi-Fi")
        XCTAssertEqual(AppModel.networkService(forDevice: "en6", serviceOrder: laptopOrder), "USB 10/100/1000 LAN")
    }

    func testFallsBackToTheFirstServiceWhenTheDeviceIsUnknown() {
        XCTAssertEqual(AppModel.networkService(forDevice: "utun3", serviceOrder: wiredOrder), "Ethernet")
        XCTAssertEqual(AppModel.networkService(forDevice: nil, serviceOrder: laptopOrder), "USB 10/100/1000 LAN")
        XCTAssertNil(AppModel.networkService(forDevice: nil, serviceOrder: ""))
    }

    func testBodyHeightNeverCollapsesAndIsCapped() {
        XCTAssertEqual(PopoverView.visibleBodyHeight(for: 0), 1)
        XCTAssertEqual(PopoverView.visibleBodyHeight(for: 312), 312)
        XCTAssertEqual(PopoverView.visibleBodyHeight(for: 2_000), PopoverView.maxBodyHeight)
    }
}

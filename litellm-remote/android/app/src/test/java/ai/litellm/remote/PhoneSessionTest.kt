package ai.litellm.remote

import org.junit.Assert.assertEquals
import org.junit.Test

class PhoneSessionTest {
    @Test fun accessTokenStaysInMemoryUntilTheRefreshWindow() {
        assertEquals(PhoneBearer.Refresh, phoneBearer(null, 0, true, 0))
        assertEquals(PhoneBearer.Current("access-1"), phoneBearer("access-1", 1_000_000, true, 1_000_000 - 180_001))
        assertEquals(PhoneBearer.Refresh, phoneBearer("access-1", 1_000_000, true, 1_000_000 - 180_000))
    }

    @Test fun legacySessionIsUsedUntilTheServerRejectsIt() {
        assertEquals(PhoneBearer.Current("week-old"), phoneBearer("week-old", 0, false, 0))
        assertEquals(PhoneBearer.SignedOut, phoneBearer(null, 0, false, 0))
    }
}

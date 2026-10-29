package se.bokheim.reader.gpui;

import static org.junit.Assert.*;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.RuntimeEnvironment;
import org.robolectric.annotation.Config;

@RunWith(RobolectricTestRunner.class)
@Config(sdk = 30)
public class GoogleDriveDisabledTest {
    @Test public void disabledBuildCannotAuthorizeEvenIfAvailabilityIsReported() {
        assertFalse(MainActivity.googleDriveAvailable(RuntimeEnvironment.getApplication()));
        assertThrows(ClassNotFoundException.class, () -> Class.forName("com.google.android.gms.auth.api.identity.Identity"));
        AtomicBoolean completed = new AtomicBoolean();
        GoogleDriveAuthorization authorization = new GoogleDriveAuthorization(RuntimeEnvironment.getApplication(),
            pending -> fail("disabled builds must not launch Google consent"), (id, token, error) -> {
                assertEquals(42, id);
                assertNull(token);
                assertEquals(GoogleDriveAuthorization.UNAVAILABLE, error);
                completed.set(true);
            });
        authorization.authorize(42, "reader@example.com", true, null, true);
        assertTrue(completed.get());
    }
}

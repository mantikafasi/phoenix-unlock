package phx;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.DialogInterface;
import android.graphics.Typeface;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;
import android.app.Fragment;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.OpenableColumns;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.lang.reflect.Field;

/**
 * Opens Android's document picker from a headless fragment and hands the chosen file's text to
 * native code (crates/runtime/src/picker.rs loads this class from an in-memory dex and registers
 * onPicked). Build: crates/runtime/java/build.sh.
 */
public final class Picker extends Fragment {
    private static final int REQ = 0x5048;
    private static final int MAX = 4 << 20;

    /** name/data are null when nothing was picked; error is null unless something failed. */
    static native void onPicked(int id, String name, byte[] data, String error);

    /** The option the user chose in a {@link #choose} dialog, or -1 when they dismissed it. */
    static native void onChosen(int id, int index);

    private int id;
    private String mime;

    public Picker() {} // fragments need a public no-arg constructor

    /** Finds the Activity held by `owner` (Phoenix's GuestHost) and opens the picker on the main thread. */
    public static void pick(final Object owner, final int id, final String mime) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                try {
                    Activity a = findActivity(owner);
                    if (a == null) { onPicked(id, null, null, "no activity"); return; }
                    Picker p = new Picker();
                    p.id = id;
                    p.mime = mime;
                    p.setRetainInstance(true); // survives rotation without being re-created by name
                    a.getFragmentManager().beginTransaction().add(p, "phx-picker-" + id).commitAllowingStateLoss();
                } catch (Throwable t) {
                    onPicked(id, null, null, String.valueOf(t));
                }
            }
        });
    }

    /** Opens `url` in the browser from the Activity held by `owner`. */
    public static void open(final Object owner, final String url) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                try {
                    findActivity(owner).startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
                } catch (Throwable t) {
                    android.util.Log.e("phoenix", "opening " + url + ": " + t);
                }
            }
        });
    }

    /**
     * A dialog over Phoenix's activity: a list of `options` without `message`, or `message` with
     * up to three buttons (the first is the positive one). Answers once through onChosen.
     */
    public static void choose(final Object owner, final int id, final String title, final String message, final String[] options) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                final boolean[] answered = {false};
                class Answer implements DialogInterface.OnClickListener {
                    final int index;
                    Answer(int index) { this.index = index; }
                    public void onClick(DialogInterface d, int which) {
                        if (answered[0]) return;
                        answered[0] = true;
                        onChosen(id, index < 0 ? which : index);
                    }
                }
                try {
                    AlertDialog.Builder b = new AlertDialog.Builder(findActivity(owner), android.R.style.Theme_DeviceDefault_Dialog_Alert);
                    if (title != null) b.setTitle(title);
                    if (message == null) {
                        b.setItems(options, new Answer(-1));
                    } else {
                        b.setMessage(message);
                        if (options.length > 0) b.setPositiveButton(options[0], new Answer(0));
                        if (options.length > 1) b.setNegativeButton(options[1], new Answer(1));
                        if (options.length > 2) b.setNeutralButton(options[2], new Answer(2));
                    }
                    b.setOnDismissListener(new DialogInterface.OnDismissListener() {
                        public void onDismiss(DialogInterface d) {
                            if (answered[0]) return;
                            answered[0] = true;
                            onChosen(id, -1);
                        }
                    });
                    b.show();
                } catch (Throwable t) {
                    android.util.Log.e("phoenix", "choose: " + t);
                    if (!answered[0]) { answered[0] = true; onChosen(id, -1); }
                }
            }
        });
    }

    public static void toast(final Object owner, final String text) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                try {
                    Toast.makeText(findActivity(owner), text, Toast.LENGTH_SHORT).show();
                } catch (Throwable t) {
                    android.util.Log.e("phoenix", "toast: " + t);
                }
            }
        });
    }

    public static void copy(final Object owner, final String text) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                try {
                    Activity a = findActivity(owner);
                    if (a == null) return;
                    ClipboardManager cm = (ClipboardManager) a.getSystemService(Context.CLIPBOARD_SERVICE);
                    if (cm != null) cm.setPrimaryClip(ClipData.newPlainText("phoenix", text));
                } catch (Throwable t) {
                    android.util.Log.e("phoenix", "copy: " + t);
                }
            }
        });
    }

    /** Scrollable selectable text/code dialog fallback when no Phoenix sheet can be drawn. */
    public static void view(final Object owner, final int id, final String title, final String text, final boolean monospace, final String[] options) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                final boolean[] answered = {false};
                class Answer implements DialogInterface.OnClickListener {
                    final int index;
                    Answer(int index) { this.index = index; }
                    public void onClick(DialogInterface d, int which) {
                        if (answered[0]) return;
                        answered[0] = true;
                        onChosen(id, index);
                    }
                }
                try {
                    Activity a = findActivity(owner);
                    float dp = a.getResources().getDisplayMetrics().density;
                    int pad = (int) (16 * dp);
                    TextView tv = new TextView(a);
                    tv.setText(text);
                    tv.setTextSize(12f);
                    tv.setTextColor(0xFFF6F6F7);
                    if (monospace) tv.setTypeface(Typeface.MONOSPACE);
                    tv.setTextIsSelectable(true);
                    tv.setPadding(pad, pad, pad, pad);
                    ScrollView sv = new ScrollView(a);
                    sv.setBackgroundColor(0xFF141518);
                    sv.addView(tv);
                    AlertDialog.Builder b = new AlertDialog.Builder(a, android.R.style.Theme_DeviceDefault_Dialog_Alert);
                    if (title != null) b.setTitle(title);
                    b.setView(sv);
                    if (options.length > 0) b.setPositiveButton(options[0], new Answer(0));
                    if (options.length > 1) b.setNegativeButton(options[1], new Answer(1));
                    if (options.length > 2) b.setNeutralButton(options[2], new Answer(2));
                    b.setOnDismissListener(new DialogInterface.OnDismissListener() {
                        public void onDismiss(DialogInterface d) {
                            if (answered[0]) return;
                            answered[0] = true;
                            onChosen(id, -1);
                        }
                    });
                    b.show();
                } catch (Throwable t) {
                    android.util.Log.e("phoenix", "view: " + t);
                    if (!answered[0]) { answered[0] = true; onChosen(id, -1); }
                }
            }
        });
    }

    static Activity findActivity(Object owner) throws IllegalAccessException {
        for (Class<?> c = owner.getClass(); c != null; c = c.getSuperclass())
            for (Field f : c.getDeclaredFields())
                if (Activity.class.isAssignableFrom(f.getType())) {
                    f.setAccessible(true);
                    Object v = f.get(owner);
                    if (v != null) return (Activity) v;
                }
        return null;
    }

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        if (state != null && mime == null) { done(); return; } // restored after process death: nobody waits
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType(mime == null ? "*/*" : mime);
        startActivityForResult(i, REQ);
    }

    @Override
    public void onActivityResult(int req, int res, Intent data) {
        if (req != REQ) return;
        final Uri uri = res == Activity.RESULT_OK && data != null ? data.getData() : null;
        final ContentResolver cr = getActivity() != null ? getActivity().getContentResolver() : null;
        final int id = this.id;
        done();
        if (uri == null || cr == null) { onPicked(id, null, null, null); return; }
        new Thread(new Runnable() {
            public void run() {
                try {
                    String name = null;
                    Cursor c = cr.query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null);
                    if (c != null) { if (c.moveToFirst()) name = c.getString(0); c.close(); }
                    InputStream in = cr.openInputStream(uri);
                    ByteArrayOutputStream out = new ByteArrayOutputStream();
                    byte[] buf = new byte[65536];
                    for (int n; (n = in.read(buf)) > 0; ) {
                        out.write(buf, 0, n);
                        if (out.size() > MAX) { in.close(); onPicked(id, name, null, "file is larger than 4 MB"); return; }
                    }
                    in.close();
                    onPicked(id, name == null ? "plugin.js" : name, out.toByteArray(), null);
                } catch (Throwable t) {
                    onPicked(id, null, null, String.valueOf(t));
                }
            }
        }, "phoenix-picker").start();
    }

    private void done() {
        if (getFragmentManager() != null) getFragmentManager().beginTransaction().remove(this).commitAllowingStateLoss();
    }
}

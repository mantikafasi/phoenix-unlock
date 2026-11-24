package phx;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Rect;
import android.graphics.Typeface;
import android.util.Base64;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.util.UUID;

/**
 * Renders a Unicode emoji sequence to a 128x128 PNG using either Android's
 * system emoji font ("system") or a custom font file path, caching the PNG at
 * outPath and returning a `data:image/png;base64,...` URI for Phoenix's image
 * loader (`jd.p`). Compiled into picker.dex by crates/runtime/build.rs.
 */
public final class EmojiDraw {
    private static final int SIZE = 128;

    private EmojiDraw() {}

    public static String draw(String codepoints, String fontPath, String outPath) {
        if (codepoints == null || codepoints.isEmpty() || outPath == null || outPath.isEmpty()) {
            return null;
        }
        File target = new File(outPath);
        if (target.isFile()) {
            byte[] cached = readFile(target);
            if (cached != null && cached.length > 0) {
                return "data:image/png;base64," + Base64.encodeToString(cached, Base64.NO_WRAP);
            }
        }

        String text = decodeCodepoints(codepoints);
        if (text == null || text.isEmpty()) return null;

        Typeface typeface = Typeface.DEFAULT;
        if (fontPath != null && !fontPath.isEmpty() && !"system".equals(fontPath)) {
            File fontFile = new File(fontPath);
            if (!fontFile.isFile()) return null;
            typeface = Typeface.createFromFile(fontFile);
        }

        Bitmap bitmap = Bitmap.createBitmap(SIZE, SIZE, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        paint.setTypeface(typeface);
        paint.setTextSize(SIZE * 0.78f);
        paint.setTextAlign(Paint.Align.CENTER);

        Rect bounds = new Rect();
        paint.getTextBounds(text, 0, text.length(), bounds);
        float maxDim = Math.max(bounds.width(), bounds.height());
        if (maxDim > SIZE * 0.92f) {
            paint.setTextSize(paint.getTextSize() * (SIZE * 0.92f / maxDim));
            paint.getTextBounds(text, 0, text.length(), bounds);
        }

        float x = SIZE / 2f;
        float y = SIZE / 2f - bounds.exactCenterY();
        canvas.drawText(text, x, y, paint);

        byte[] png;
        try {
            ByteArrayOutputStream baos = new ByteArrayOutputStream();
            if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, baos)) return null;
            png = baos.toByteArray();
        } finally {
            bitmap.recycle();
        }

        writeFileAtomic(target, png);
        return "data:image/png;base64," + Base64.encodeToString(png, Base64.NO_WRAP);
    }

    private static void writeFileAtomic(File target, byte[] bytes) {
        File parent = target.getParentFile();
        if (parent != null && !parent.isDirectory()) parent.mkdirs();
        File tmp = new File(parent, ".emoji-" + UUID.randomUUID() + ".tmp");
        try (FileOutputStream stream = new FileOutputStream(tmp)) {
            stream.write(bytes);
        } catch (Throwable t) {
            tmp.delete();
            return;
        }
        if (!tmp.renameTo(target)) tmp.delete();
    }

    private static byte[] readFile(File file) {
        try (FileInputStream in = new FileInputStream(file)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
            return out.toByteArray();
        } catch (Throwable t) {
            return null;
        }
    }

    private static String decodeCodepoints(String codepoints) {
        StringBuilder sb = new StringBuilder();
        for (String part : codepoints.split("-")) {
            if (part.isEmpty()) return null;
            int cp;
            try {
                cp = Integer.parseInt(part, 16);
            } catch (NumberFormatException e) {
                return null;
            }
            if (cp < 0 || cp > 0x10FFFF) return null;
            sb.append(Character.toChars(cp));
        }
        return sb.toString();
    }
}

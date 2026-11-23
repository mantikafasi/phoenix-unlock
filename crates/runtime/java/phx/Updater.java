package phx;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.util.Base64;
import android.util.Log;
import java.io.BufferedOutputStream;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.io.RandomAccessFile;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.security.KeyFactory;
import java.security.MessageDigest;
import java.security.PrivateKey;
import java.security.Signature;
import java.security.interfaces.RSAKey;
import java.security.spec.PKCS8EncodedKeySpec;
import java.util.ArrayList;
import java.util.List;
import java.util.zip.CRC32;

/**
 * In-app APK self-updater: streams the currently installed merged APK from
 * ApplicationInfo.sourceDir, swaps only lib/<abi>/libplm.so with the newly downloaded
 * loader (16 KiB aligned, zero re-compression), signs with APK Signature Scheme v3
 * (matching web/lib/sign.js), and launches Android's PackageInstaller confirmation prompt.
 */
public final class Updater {
    private static final String TAG = "phoenix";
    private static final String LOADER_NAME = "libplm.so";
    private static final int V3_BLOCK_ID = 0xf05368c0;
    private static final int PROOF_OF_ROTATION = 0x3ba06f8c;
    private static final int ROTATION_MIN_SDK = 28;
    private static final int MAX_SDK = 0x7fffffff;
    private static final int CHUNK_SIZE = 1 << 20; // 1 MiB
    private static final int ALG_SHA256 = 0x0103;
    private static final int ALG_SHA512 = 0x0104;

    static native void onUpdateStatus(String state, String message);

    private static void report(String state, String message) {
        try {
            onUpdateStatus(state, message);
        } catch (Throwable t) {
            Log.e(TAG, "onUpdateStatus failed: " + t);
        }
    }

    public static void selfUpdate(
            final Object owner,
            final byte[] newLoader,
            final String pkcs8B64,
            final String spkiB64,
            final String certB64,
            final String lineageB64,
            final String fallbackUrl) {
        new Thread(new Runnable() {
            public void run() {
                File outApk = null;
                try {
                    final Activity activity = Picker.findActivity(owner);
                    if (activity == null) {
                        report("error", "No active window");
                        return;
                    }
                    final Context ctx = activity.getApplicationContext();
                    final ApplicationInfo ai = ctx.getApplicationInfo();
                    if (ai.splitSourceDirs != null && ai.splitSourceDirs.length > 0) {
                        report("fallback", "Split APK install detected — opening download page");
                        Picker.open(owner, fallbackUrl);
                        return;
                    }
                    if (!hasInstallPermissionDeclared(ctx)) {
                        report("fallback", "Re-patch once from the site to enable 1-tap updates");
                        Picker.open(owner, fallbackUrl);
                        return;
                    }
                    if (Build.VERSION.SDK_INT >= 26 && !ctx.getPackageManager().canRequestPackageInstalls()) {
                        report("permission", "Allow 'Install unknown apps' for Discord, then tap Update again");
                        new Handler(Looper.getMainLooper()).post(new Runnable() {
                            public void run() {
                                try {
                                    Intent i = new Intent(
                                            Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                            Uri.parse("package:" + ctx.getPackageName()));
                                    activity.startActivity(i);
                                } catch (Throwable t) {
                                    Log.e(TAG, "open unknown sources failed: " + t);
                                }
                            }
                        });
                        return;
                    }

                    report("repacking", "Repacking & signing APK...");
                    outApk = new File(ctx.getCacheDir(), "phoenix-update.apk");
                    repackAndSign(
                            new File(ai.sourceDir),
                            outApk,
                            newLoader,
                            Base64.decode(pkcs8B64, Base64.DEFAULT),
                            Base64.decode(spkiB64, Base64.DEFAULT),
                            Base64.decode(certB64, Base64.DEFAULT),
                            Base64.decode(lineageB64, Base64.DEFAULT));

                    report("installing", "Opening Android installer...");
                    commitPackageInstaller(activity, ctx, outApk);
                } catch (Throwable t) {
                    Log.e(TAG, "selfUpdate failed", t);
                    report("error", "Self-update failed: " + t.getMessage());
                } finally {
                    if (outApk != null && outApk.exists()) {
                        //noinspection ResultOfMethodCallIgnored
                        outApk.delete();
                    }
                }
            }
        }, "phoenix-updater").start();
    }

    private static boolean hasInstallPermissionDeclared(Context ctx) {
        try {
            PackageInfo pi = ctx.getPackageManager().getPackageInfo(
                    ctx.getPackageName(), PackageManager.GET_PERMISSIONS);
            if (pi.requestedPermissions == null) return false;
            for (String p : pi.requestedPermissions) {
                if ("android.permission.REQUEST_INSTALL_PACKAGES".equals(p)) return true;
            }
        } catch (Throwable t) {
            Log.w(TAG, "permission check failed: " + t);
        }
        return false;
    }

    private static void commitPackageInstaller(final Activity activity, final Context ctx, File apkFile) throws IOException {
        PackageInstaller pi = ctx.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params =
                new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(ctx.getPackageName());
        params.setSize(apkFile.length());
        final int sessionId = pi.createSession(params);
        PackageInstaller.Session session = pi.openSession(sessionId);
        try {
            OutputStream out = session.openWrite("base.apk", 0, apkFile.length());
            FileInputStream in = new FileInputStream(apkFile);
            try {
                byte[] buf = new byte[1 << 17];
                int n;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                }
                session.fsync(out);
            } finally {
                in.close();
                out.close();
            }

            final String action = "phx.UPDATE_STATUS." + sessionId;
            final BroadcastReceiver receiver = new BroadcastReceiver() {
                @Override
                public void onReceive(Context context, Intent intent) {
                    int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
                    String msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
                    if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                        final Intent confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT);
                        if (confirm != null) {
                            new Handler(Looper.getMainLooper()).post(new Runnable() {
                                public void run() {
                                    try {
                                        activity.startActivity(confirm);
                                    } catch (Throwable t) {
                                        try {
                                            confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                                            ctx.startActivity(confirm);
                                        } catch (Throwable t2) {
                                            report("error", "Could not launch installer: " + t2.getMessage());
                                        }
                                    }
                                }
                            });
                        }
                        return;
                    }
                    try {
                        ctx.unregisterReceiver(this);
                    } catch (Throwable ignored) {}
                    if (status == PackageInstaller.STATUS_SUCCESS) {
                        report("done", "Update installed");
                    } else if (status == PackageInstaller.STATUS_FAILURE_ABORTED) {
                        report("cancelled", "Update cancelled");
                    } else {
                        report("error", "Installer failed (" + status + "): " + (msg == null ? "unknown" : msg));
                    }
                }
            };
            IntentFilter filter = new IntentFilter(action);
            if (Build.VERSION.SDK_INT >= 33) {
                ctx.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED);
            } else {
                ctx.registerReceiver(receiver, filter);
            }

            Intent callbackIntent = new Intent(action).setPackage(ctx.getPackageName());
            int flags = PendingIntent.FLAG_UPDATE_CURRENT;
            if (Build.VERSION.SDK_INT >= 31) {
                flags |= PendingIntent.FLAG_MUTABLE;
            }
            PendingIntent pending = PendingIntent.getBroadcast(ctx, sessionId, callbackIntent, flags);
            session.commit(pending.getIntentSender());
        } finally {
            session.close();
        }
    }

    private static final class ZipEntryMeta {
        String name;
        byte[] nameBytes;
        int method;
        int crc;
        long csize;
        long usize;
        long dataOffset;
        long newLocalOffset;
    }

    /** Streaming 1 MiB APK Signature Scheme v2/v3 chunk digester. */
    private static final class ChunkHasher {
        private final String alg;
        private final MessageDigest chunkMd;
        private final byte[] buf = new byte[CHUNK_SIZE];
        private int fill = 0;
        private final List<byte[]> digests = new ArrayList<byte[]>();

        ChunkHasher(String alg) throws Exception {
            this.alg = alg;
            this.chunkMd = MessageDigest.getInstance(alg);
        }

        void update(byte[] data, int off, int len) {
            int p = off;
            int end = off + len;
            while (p < end) {
                int n = Math.min(CHUNK_SIZE - fill, end - p);
                System.arraycopy(data, p, buf, fill, n);
                fill += n;
                p += n;
                if (fill == CHUNK_SIZE) {
                    flushChunk();
                }
            }
        }

        void update(byte[] data) {
            update(data, 0, data.length);
        }

        void finishSection() {
            if (fill > 0) {
                flushChunk();
            }
        }

        private void flushChunk() {
            chunkMd.reset();
            chunkMd.update((byte) 0xa5);
            chunkMd.update(u32le(fill));
            chunkMd.update(buf, 0, fill);
            digests.add(chunkMd.digest());
            fill = 0;
        }

        byte[] finishRoot() throws Exception {
            finishSection();
            MessageDigest root = MessageDigest.getInstance(alg);
            root.update((byte) 0x5a);
            root.update(u32le(digests.size()));
            for (byte[] d : digests) {
                root.update(d);
            }
            return root.digest();
        }
    }

    private static void repackAndSign(
            File srcApk,
            File dstApk,
            byte[] newLoader,
            byte[] pkcs8,
            byte[] spki,
            byte[] cert,
            byte[] lineage) throws Exception {
        PrivateKey privKey = KeyFactory.getInstance("RSA").generatePrivate(new PKCS8EncodedKeySpec(pkcs8));
        int modBits = ((RSAKey) privKey).getModulus().bitLength();
        String hashName = modBits > 3072 ? "SHA-512" : "SHA-256";
        String sigAlgName = modBits > 3072 ? "SHA512withRSA" : "SHA256withRSA";
        int sigAlgId = modBits > 3072 ? ALG_SHA512 : ALG_SHA256;

        RandomAccessFile raf = new RandomAccessFile(srcApk, "r");
        try {
            List<ZipEntryMeta> entries = readCentralDirectory(raf);
            int replacedCount = 0;
            CRC32 loaderCrcCalc = new CRC32();
            loaderCrcCalc.update(newLoader);
            int loaderCrc = (int) loaderCrcCalc.getValue();

            ChunkHasher hasher = new ChunkHasher(hashName);
            List<ZipEntryMeta> kept = new ArrayList<ZipEntryMeta>(entries.size());
            long outOffset = 0;
            byte[] copyBuf = new byte[1 << 17];

            OutputStream fos = new BufferedOutputStream(new FileOutputStream(dstApk), 1 << 17);
            try {
                // Section 1: stream all entries, replacing lib/<abi>/libplm.so
                for (ZipEntryMeta e : entries) {
                    if (isStaleSig(e.name)) continue;
                    boolean replace = e.name.startsWith("lib/") && e.name.endsWith("/" + LOADER_NAME);
                    int method = replace ? 0 : e.method;
                    int crc = replace ? loaderCrc : e.crc;
                    long csize = replace ? newLoader.length : e.csize;
                    long usize = replace ? newLoader.length : e.usize;

                    int align = alignFor(e.name, method);
                    int pad = align == 0 ? 0 : (int) ((align - ((outOffset + 30 + e.nameBytes.length) % align)) % align);

                    byte[] lh = new byte[30 + e.nameBytes.length + pad];
                    ByteBuffer lb = ByteBuffer.wrap(lh).order(ByteOrder.LITTLE_ENDIAN);
                    lb.putInt(0, 0x04034b50);
                    lb.putShort(4, (short) (method == 0 ? 10 : 20));
                    lb.putShort(6, (short) 0);
                    lb.putShort(8, (short) method);
                    lb.putInt(10, 0x00210000);
                    lb.putInt(14, crc);
                    lb.putInt(18, (int) csize);
                    lb.putInt(22, (int) usize);
                    lb.putShort(26, (short) e.nameBytes.length);
                    lb.putShort(28, (short) pad);
                    System.arraycopy(e.nameBytes, 0, lh, 30, e.nameBytes.length);

                    ZipEntryMeta outEntry = new ZipEntryMeta();
                    outEntry.name = e.name;
                    outEntry.nameBytes = e.nameBytes;
                    outEntry.method = method;
                    outEntry.crc = crc;
                    outEntry.csize = csize;
                    outEntry.usize = usize;
                    outEntry.newLocalOffset = outOffset;
                    kept.add(outEntry);

                    fos.write(lh);
                    hasher.update(lh);
                    outOffset += lh.length;

                    if (replace) {
                        fos.write(newLoader);
                        hasher.update(newLoader);
                        outOffset += newLoader.length;
                        replacedCount++;
                    } else {
                        raf.seek(e.dataOffset);
                        long rem = csize;
                        while (rem > 0) {
                            int n = raf.read(copyBuf, 0, (int) Math.min(copyBuf.length, rem));
                            if (n <= 0) throw new IOException("unexpected EOF copying " + e.name);
                            fos.write(copyBuf, 0, n);
                            hasher.update(copyBuf, 0, n);
                            rem -= n;
                            outOffset += n;
                        }
                    }
                }
                if (replacedCount == 0) {
                    throw new IOException("installed APK has no " + LOADER_NAME);
                }
                hasher.finishSection();

                long preBlockCdOffset = outOffset;

                // Section 3: Central Directory
                ByteArrayOutputStream cdOut = new ByteArrayOutputStream(kept.size() * 80);
                for (ZipEntryMeta e : kept) {
                    byte[] c = new byte[46 + e.nameBytes.length];
                    ByteBuffer cb = ByteBuffer.wrap(c).order(ByteOrder.LITTLE_ENDIAN);
                    cb.putInt(0, 0x02014b50);
                    cb.putShort(4, (short) 20);
                    cb.putShort(6, (short) (e.method == 0 ? 10 : 20));
                    cb.putShort(10, (short) e.method);
                    cb.putInt(12, 0x00210000);
                    cb.putInt(16, e.crc);
                    cb.putInt(20, (int) e.csize);
                    cb.putInt(24, (int) e.usize);
                    cb.putShort(28, (short) e.nameBytes.length);
                    cb.putInt(42, (int) e.newLocalOffset);
                    System.arraycopy(e.nameBytes, 0, c, 46, e.nameBytes.length);
                    cdOut.write(c);
                }
                byte[] cdBytes = cdOut.toByteArray();
                hasher.update(cdBytes);
                hasher.finishSection();

                // Section 4: EOCD (hashed with pre-block cdOffset, per APK Sig Scheme v2/v3 spec)
                byte[] eocd = buildEocd(kept.size(), cdBytes.length, preBlockCdOffset);
                hasher.update(eocd);
                byte[] contentDigest = hasher.finishRoot();

                // Build APK Signing Block v3 (matches web/lib/sign.js)
                byte[] sigBlock = buildV3SigningBlock(
                        privKey, sigAlgName, sigAlgId, contentDigest, cert, spki, lineage);

                byte[] finalEocd = buildEocd(kept.size(), cdBytes.length, preBlockCdOffset + sigBlock.length);

                fos.write(sigBlock);
                fos.write(cdBytes);
                fos.write(finalEocd);
                fos.flush();
            } finally {
                fos.close();
            }
        } finally {
            raf.close();
        }
    }

    private static List<ZipEntryMeta> readCentralDirectory(RandomAccessFile raf) throws IOException {
        long fileLen = raf.length();
        int searchLen = (int) Math.min(fileLen, 65557L);
        byte[] tail = new byte[searchLen];
        raf.seek(fileLen - searchLen);
        raf.readFully(tail);
        ByteBuffer tb = ByteBuffer.wrap(tail).order(ByteOrder.LITTLE_ENDIAN);
        int eocdPos = -1;
        for (int i = searchLen - 22; i >= 0; i--) {
            if (tb.getInt(i) == 0x06054b50) {
                eocdPos = i;
                break;
            }
        }
        if (eocdPos < 0) throw new IOException("not a zip (no EOCD)");
        int count = tb.getShort(eocdPos + 10) & 0xffff;
        int cdSize = tb.getInt(eocdPos + 12);
        long cdOff = tb.getInt(eocdPos + 16) & 0xffffffffL;

        byte[] cd = new byte[cdSize];
        raf.seek(cdOff);
        raf.readFully(cd);
        ByteBuffer cb = ByteBuffer.wrap(cd).order(ByteOrder.LITTLE_ENDIAN);

        List<ZipEntryMeta> list = new ArrayList<ZipEntryMeta>(count);
        byte[] lhHead = new byte[30];
        ByteBuffer lhb = ByteBuffer.wrap(lhHead).order(ByteOrder.LITTLE_ENDIAN);
        int p = 0;
        for (int i = 0; i < count; i++) {
            if (cb.getInt(p) != 0x02014b50) throw new IOException("bad central directory");
            ZipEntryMeta e = new ZipEntryMeta();
            e.method = cb.getShort(p + 10) & 0xffff;
            e.crc = cb.getInt(p + 16);
            e.csize = cb.getInt(p + 20) & 0xffffffffL;
            e.usize = cb.getInt(p + 24) & 0xffffffffL;
            int nlen = cb.getShort(p + 28) & 0xffff;
            int xlen = cb.getShort(p + 30) & 0xffff;
            int clen = cb.getShort(p + 32) & 0xffff;
            long lho = cb.getInt(p + 42) & 0xffffffffL;
            e.nameBytes = new byte[nlen];
            System.arraycopy(cd, p + 46, e.nameBytes, 0, nlen);
            e.name = new String(e.nameBytes, StandardCharsets.UTF_8);

            raf.seek(lho);
            raf.readFully(lhHead);
            int lnl = lhb.getShort(26) & 0xffff;
            int lxl = lhb.getShort(28) & 0xffff;
            e.dataOffset = lho + 30 + lnl + lxl;
            list.add(e);
            p += 46 + nlen + xlen + clen;
        }
        return list;
    }

    private static boolean isStaleSig(String name) {
        if ("stamp-cert-sha256".equals(name)) return true;
        if (!name.startsWith("META-INF/")) return false;
        String rest = name.substring("META-INF/".length());
        if (rest.indexOf('/') >= 0) return false;
        return "MANIFEST.MF".equals(rest)
                || rest.endsWith(".SF")
                || rest.endsWith(".RSA")
                || rest.endsWith(".DSA")
                || rest.endsWith(".EC");
    }

    private static int alignFor(String name, int method) {
        if (method != 0) return 0;
        return name.endsWith(".so") ? 16384 : 4;
    }

    private static byte[] buildEocd(int count, int cdSize, long cdOffset) {
        byte[] eocd = new byte[22];
        ByteBuffer b = ByteBuffer.wrap(eocd).order(ByteOrder.LITTLE_ENDIAN);
        b.putInt(0, 0x06054b50);
        b.putShort(8, (short) count);
        b.putShort(10, (short) count);
        b.putInt(12, cdSize);
        b.putInt(16, (int) cdOffset);
        return eocd;
    }

    private static byte[] buildV3SigningBlock(
            PrivateKey key,
            String sigAlgName,
            int algId,
            byte[] digest,
            byte[] cert,
            byte[] spki,
            byte[] lineage) throws Exception {
        byte[] sdk = cat(u32le(ROTATION_MIN_SDK), u32le(MAX_SDK));
        byte[] attrs = lp(cat(u32le(PROOF_OF_ROTATION), lineage));
        byte[] signedData = cat(
                lp(lp(cat(u32le(algId), lp(digest)))),
                lp(lp(cert)),
                sdk,
                lp(attrs));
        Signature sig = Signature.getInstance(sigAlgName);
        sig.initSign(key);
        sig.update(signedData);
        byte[] sigBytes = sig.sign();

        byte[] v3 = lp(lp(cat(
                lp(signedData),
                sdk,
                lp(lp(cat(u32le(algId), lp(sigBytes)))),
                lp(spki))));
        byte[] pair = cat(u64le(4L + v3.length), u32le(V3_BLOCK_ID), v3);
        byte[] magic = "APK Sig Block 42".getBytes(StandardCharsets.US_ASCII);
        long blockSize = pair.length + 8L + magic.length;
        return cat(u64le(blockSize), pair, u64le(blockSize), magic);
    }

    private static byte[] u32le(int v) {
        return ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(v).array();
    }

    private static byte[] u64le(long v) {
        return ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(v).array();
    }

    private static byte[] lp(byte[] b) {
        return cat(u32le(b.length), b);
    }

    private static byte[] cat(byte[]... parts) {
        int total = 0;
        for (byte[] p : parts) total += p.length;
        byte[] out = new byte[total];
        int pos = 0;
        for (byte[] p : parts) {
            System.arraycopy(p, 0, out, pos, p.length);
            pos += p.length;
        }
        return out;
    }
}

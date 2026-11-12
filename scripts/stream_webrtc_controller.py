"""Private pipe controller for synthetic WebRTC diagnostics; never run in a terminal.

SDP and ICE go only to the owning Rust subprocess pipe. No browser profile,
screen, microphone, camera, personal session or external STUN service is used.
Requires separately installed Playwright and Chromium. Installs nothing.
"""
import json
from importlib.metadata import version
import os
from pathlib import Path
import sys

MAX_LINE = 32768


def read_command(stream):
    line = stream.readline(MAX_LINE + 1)
    if not line:
        return None
    if len(line) > MAX_LINE or not line.endswith(b"\n"):
        raise ValueError("invalid controller frame")
    value = json.loads(line)
    if not isinstance(value, dict) or not isinstance(value.get("op"), str):
        raise ValueError("invalid controller command")
    return value


INIT = """role => {
  window.pc = new RTCPeerConnection({iceServers: []});
  window.pending = [];
  window.candidateOverflow = false;
  pc.onicecandidate = e => {
    if (e.candidate) {
      if (pending.length >= 64) window.candidateOverflow = true;
      else pending.push(e.candidate.toJSON());
    }
  };
  window.video = document.createElement('video');
  video.autoplay = true; video.muted = true; video.playsInline = true;
  document.body.append(video);
  pc.ontrack = e => {
    video.srcObject = e.streams[0] || new MediaStream([e.track]);
    video.play().catch(() => {});
  };
  let transceiver;
  if (role === 'publisher') {
    window.canvas = document.createElement('canvas');
    canvas.width = 320; canvas.height = 180;
    const ctx = canvas.getContext('2d'); let counter = 0;
    const paint = () => {
      ctx.fillStyle = 'rgb(20,180,80)'; ctx.fillRect(0,0,320,180);
      ctx.fillStyle = 'black'; ctx.fillRect((counter++ % 20)*10,0,8,8);
    };
    paint(); window.paintTimer = setInterval(paint,100);
    window.source = canvas.captureStream(10);
    transceiver = pc.addTransceiver(source.getVideoTracks()[0],
      {direction: 'sendonly', streams: [source]});
    pc.addTransceiver('audio', {direction: 'inactive'});
  } else {
    transceiver = pc.addTransceiver('video', {direction: 'recvonly'});
  }
  const vp8 = RTCRtpSender.getCapabilities('video').codecs
    .filter(c => c.mimeType.toLowerCase() === 'video/vp8');
  if (!vp8.length) throw new Error('VP8 unavailable');
  transceiver.setCodecPreferences(vp8);
}"""

STRIP_CANDIDATES = """sdp => sdp.split(/\\r?\\n/)
  .filter(s => s && !s.startsWith('a=candidate:') && s !== 'a=end-of-candidates')
  .join('\\r\\n') + '\\r\\n'"""


class BrowserPeers:
    def __init__(self):
        from playwright.sync_api import sync_playwright
        self.playwright = sync_playwright().start()
        self.browser = None
        try:
            self.browser = self.playwright.chromium.launch(
                headless=True, args=["--autoplay-policy=no-user-gesture-required"])
            self.pages = {}
            for role in ("publisher", "viewer"):
                page = self.browser.new_page()
                page.set_content("<!doctype html><title>Mirage synthetic fixture</title>")
                page.evaluate(INIT, role)
                self.pages[role] = page
        except Exception:
            self.close()
            raise

    def call(self, command):
        op = command["op"]
        if op == "offer":
            page = self.pages["publisher"]
            sdp = page.evaluate("""async () => {
                const offer = await pc.createOffer();
                await pc.setLocalDescription(offer); return offer.sdp;
            }""")
            return {"sdp": page.evaluate(STRIP_CANDIDATES, sdp)}
        if op == "accept_offer":
            page = self.pages["viewer"]
            sdp = page.evaluate("""async sdp => {
                await pc.setRemoteDescription({type:'offer',sdp});
                const answer = await pc.createAnswer();
                await pc.setLocalDescription(answer); return answer.sdp;
            }""", command["sdp"])
            return {"sdp": page.evaluate(STRIP_CANDIDATES, sdp)}
        if op == "accept_answer":
            self.pages["publisher"].evaluate(
                "sdp => pc.setRemoteDescription({type:'answer',sdp})", command["sdp"])
            return {}
        if op == "candidates":
            return {role: page.evaluate("() => { if (candidateOverflow) throw new Error('candidate overflow'); return pending.splice(0); }")
                    for role, page in self.pages.items()}
        if op == "ice":
            self.pages[command["role"]].evaluate("""data => pc.addIceCandidate({
                candidate:data.sdp, sdpMid:data.mid, sdpMLineIndex:data.mLine
            })""", command["candidate"])
            return {}
        if op == "stats":
            return self.pages["viewer"].evaluate("""async () => {
                const reports = await pc.getStats(); let decoded = 0;
                reports.forEach(r => {
                  if (r.type === 'inbound-rtp' && r.kind === 'video')
                    decoded += r.framesDecoded || 0;
                });
                let fixture = false;
                if (video.videoWidth && video.readyState >= 2) {
                  const c = document.createElement('canvas'); c.width=1;c.height=1;
                  const ctx=c.getContext('2d');ctx.drawImage(video,160,90,1,1,0,0,1,1);
                  const p=ctx.getImageData(0,0,1,1).data;
                  fixture=Math.abs(p[0]-20)<30 && Math.abs(p[1]-180)<30 && Math.abs(p[2]-80)<30;
                }
                return {frames_decoded:decoded,width:video.videoWidth,height:video.videoHeight,
                  fixture_pixel_matches:fixture,connection_state:pc.connectionState};
            }""")
        if op == "snapshot":
            destination = Path(os.environ["TMPDIR"]) / "received-fixture.png"
            self.pages["viewer"].locator("video").screenshot(path=str(destination), timeout=5000)
            return {}
        raise ValueError("unknown controller operation")

    def close(self):
        if self.browser:
            self.browser.close()
        self.playwright.stop()


def main():
    peers = None
    try:
        while (command := read_command(sys.stdin.buffer)) is not None:
            try:
                if command["op"] == "init":
                    if peers is not None:
                        raise ValueError("already initialized")
                    peers = BrowserPeers()
                    data = {}
                elif command["op"] == "shutdown":
                    if peers:
                        peers.close()
                        peers = None
                    print(json.dumps({"ok": True, "data": {}}), flush=True)
                    return
                else:
                    if peers is None:
                        raise ValueError("not initialized")
                    data = peers.call(command)
                    if command["op"] == "stats":
                        data["chromium_version"] = peers.browser.version
                        data["playwright_version"] = version("playwright")
                response = json.dumps({"ok": True, "data": data})
                if len(response.encode()) > MAX_LINE - 1:
                    raise ValueError("controller response exceeds limit")
                print(response, flush=True)
            except Exception:
                print('{"ok":false,"error":"controller_operation_failed"}', flush=True)
                return
    finally:
        if peers:
            peers.close()


if __name__ == "__main__":
    try:
        main()
    except Exception:
        print('{"ok":false,"error":"controller_input_failed"}', flush=True)
        raise SystemExit(1) from None

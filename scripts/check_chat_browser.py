"""Manual rendered channel-chat regression with installed Playwright/Chromium.

Requires a trusted loopback TS6 server. Uses isolated bridges and no audio.
"""
import argparse
import json
from pathlib import Path
from playwright.sync_api import sync_playwright, expect
from check_channel_chat import Peer

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--server-port', type=int, default=9987)
    args = parser.parse_args()
    if not 1 <= args.server_port <= 65535:
        parser.error('invalid server port')
    root = Path(__file__).resolve().parent.parent
    children = []
    try:
        peers = [Peer(args.binary, name, children) for name in ('browser-a', 'browser-b')]
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            pages = [browser.new_page(viewport={'width': 1440, 'height': 1000}) for _ in peers]
            errors = []
            for index, (page, peer) in enumerate(zip(pages, peers)):
                page.on('pageerror', lambda error: errors.append('pageerror'))
                page.on('console', lambda message: errors.append('console-error') if message.type == 'error' and '404' not in message.text else None)
                page.goto(f'http://127.0.0.1:{peer.port}')
                expect(page.get_by_role('heading', name='Connect to a server')).to_be_visible()
                page.get_by_label('Address', exact=True).fill(f'127.0.0.1:{args.server_port}')
                page.get_by_label('Nickname', exact=True).fill(f'browser-chat-{index}')
                page.get_by_role('button', name='Connect', exact=True).click()
                expect(page.get_by_label('Channel message', exact=True)).to_be_enabled(timeout=35000)
            a, b = pages
            fixture = '<script>window.fixture_executed=true</script> & channel fixture ' + chr(128512)
            a.get_by_label('Channel message', exact=True).fill(fixture)
            a.get_by_role('button', name='Send', exact=True).click()
            for page in pages:
                expect(page.get_by_role('log', name='Channel messages').get_by_text(fixture, exact=True)).to_have_count(1, timeout=10000)
                assert page.evaluate('window.fixture_executed') is None
            expect(a.get_by_label('Channel message', exact=True)).to_have_value('')
            b.get_by_label('Channel message', exact=True).fill('Reply by Enter')
            b.get_by_label('Channel message', exact=True).press('Enter')
            expect(a.get_by_role('log', name='Channel messages').get_by_text('Reply by Enter', exact=True)).to_have_count(1)
            a.get_by_label('Channel message', exact=True).fill('x' * 1025)
            expect(a.get_by_role('button', name='Send', exact=True)).to_be_disabled()
            expect(a.get_by_label('Channel message', exact=True)).to_have_value('x' * 1025)
            a.get_by_label('Channel message', exact=True).fill('')
            a.screenshot(path=str(root / '.audit-local/chat-browser-desktop.png'))
            a.reload()
            expect(a.get_by_label('Channel message', exact=True)).to_be_enabled(timeout=10000)
            a.get_by_label('Channel message', exact=True).fill('After reload')
            a.get_by_label('Channel message', exact=True).press('Enter')
            expect(b.get_by_role('log', name='Channel messages').get_by_text('After reload', exact=True)).to_have_count(1)
            a.set_viewport_size({'width': 900, 'height': 700})
            a.screenshot(path=str(root / '.audit-local/chat-browser-compact.png'))
            assert not errors
            print(json.dumps({'rendered_channel_roundtrip': True, 'literal_markup': True, 'enter_send': True, 'oversize_draft_retained': True, 'reload_resynchronization': True, 'page_errors': len(errors)}))
            browser.close()
    finally:
        for peer in children:
            peer.close()
        print('BROWSER_TEST_CHILDREN_STOPPED')
if __name__ == '__main__':
    main()

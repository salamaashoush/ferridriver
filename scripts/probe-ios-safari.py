import argparse
import http.server
import json
import pathlib
import shlex
import subprocess
import tempfile
import threading
import time


HTML = (pathlib.Path(__file__).parent / 'fixtures/portable-form.html').read_bytes()


class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = {
            '/frames': b'<iframe name="child" srcdoc="<p>child</p>"></iframe>',
            '/empty': b'<title>No frames</title>',
        }.get(self.path, HTML)
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def simulator_inventory(options, root, stage):
    ssh = shlex.split(options.ssh_command)
    listing = subprocess.run(ssh + ['xcrun simctl list devices --json'],
                             capture_output=True, text=True, check=True, timeout=30)
    (root / f'devices-{stage}.json').write_text(listing.stdout)
    devices = {device['udid']: (device['name'], device['state'])
               for runtime in json.loads(listing.stdout)['devices'].values() for device in runtime}
    processes = subprocess.run(ssh + ['ps -axo pid=,comm='],
                               capture_output=True, text=True, check=True, timeout=30)
    interfaces = sorted(line.strip() for line in processes.stdout.splitlines()
                        if '/Simulator.app/Contents/MacOS/Simulator' in line)
    builds = sorted(line.strip() for line in processes.stdout.splitlines()
                    if line.rstrip().endswith('/xcodebuild'))
    (root / f'simulator-ui-{stage}.json').write_text(json.dumps(interfaces, indent=2))
    (root / f'driver-builds-{stage}.json').write_text(json.dumps(builds, indent=2))
    return {'devices': devices, 'interfaces': interfaces, 'builds': builds}


def run(options):
    repo = pathlib.Path(__file__).resolve().parent.parent
    root = pathlib.Path(tempfile.mkdtemp(prefix='ferridriver-ios-safari-'))
    source_path = pathlib.Path(options.workflow) if options.workflow else repo / f'scripts/fixtures/portable-{options.scenario}-workflow.js'
    source = source_path.read_text()
    if options.diagnostics:
        source = '''try { return await (async()=>{
''' + source + '''
})(); } catch(error) {
  try {
    console.error('[probe-failure]', JSON.stringify(await page.evaluate(()=>({
      url:location.href, input:document.querySelector('input')?.value,
      result:document.querySelector('#result')?.textContent, click:window.lastClick,
      viewport:{width:innerWidth,height:innerHeight,scale:visualViewport?.scale},
      active:document.activeElement?.outerHTML
    }))));
    await page.screenshot({path:args[1]});
  } catch(diagnosticError) { console.error('[probe-failure-capture]', String(diagnosticError)); }
  throw error;
}'''
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    tunnel = subprocess.Popen(shlex.split(options.ssh_command) + [
        '-o', 'ExitOnForwardFailure=yes', '-R',
        f'127.0.0.1:{options.remote_port}:127.0.0.1:{server.server_port}',
        'echo READY; cat',
    ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        if tunnel.stdout.readline().strip() != 'READY':
            raise RuntimeError('Fixture tunnel failed to start')
        capabilities = {'platformName': 'iOS', 'appium:automationName': 'XCUITest',
                        'appium:udid': options.device, 'appium:noReset': True}
        if options.log_protocol:
            capabilities['appium:safariLogAllCommunication'] = True
        target = {'browser': 'safari', 'headless': False, 'connectUrl': options.endpoint,
                  'connectOptions': {'timeout': 120000, 'capabilities': capabilities}}
        if options.managed:
            device = {'platform': 'ios', 'timeout': options.launch_timeout}
            if options.model:
                device['model'] = options.model
            if options.ios_version:
                device['version'] = options.ios_version
            target = {'device': device, 'headless': options.headless}
        config = root / 'ferridriver.json'
        config.write_text(json.dumps({'browser': {'instances': {'target': target}},
                                     'test': {'workers': 1, 'testMatch': ['workflow.test.ts'],
                                              'browser': {'instance': 'target'}}}))
        workflow = root / 'workflow.js'
        workflow.write_text(source)
        args = [f'http://127.0.0.1:{options.remote_port}/', str(root / 'safari.png'),
                options.endpoint, json.dumps(capabilities)]
        remote_root = None
        if options.remote_binary:
            remote_root = subprocess.check_output(shlex.split(options.ssh_command) + [
                'mktemp -d /tmp/ferridriver-ios-safari.XXXXXX',
            ], text=True).strip()
            remote_config = remote_root + '/ferridriver.json'
            subprocess.run(shlex.split(options.ssh_command) + [
                'set -C; cat > ' + shlex.quote(remote_config),
            ], input=config.read_text(), text=True, check=True)
            config = pathlib.Path(remote_config)
            args[1] = remote_root + '/safari.png'
        if options.runtime in ('script', 'test'):
            binary = options.remote_binary or str(repo / 'target/debug/ferridriver')
            if options.runtime == 'script':
                command = [binary, 'run', '--no-inherit', '--config', str(config),
                           '--instance', 'target', '--fresh', '--json', '--trace', '-e', source, '--', *args]
            else:
                test_root = pathlib.Path(remote_root) if remote_root else root
                result_path = str(test_root / 'workflow-result.json')
                test_source = ("import {test} from '@ferridriver/test';\n"
                               "import {writeFileSync} from 'node:fs';\n"
                               "async function workflow(page, context, browser, args) {\n" + source + "\n}\n"
                               "test('configured target runs the shared browser workflow', async ({page, context, browser}) => {\n"
                               "const value = await workflow(page, context, browser, " + json.dumps(args) + ");\n"
                               "writeFileSync(" + json.dumps(result_path) + ", JSON.stringify({status:'ok', value}));\n});\n")
                test_file = test_root / 'workflow.test.ts'
                (root / 'workflow.test.ts').write_text(test_source)
                if remote_root:
                    subprocess.run(shlex.split(options.ssh_command) + [
                        'set -C; cat > ' + shlex.quote(str(test_file)),
                    ], input=test_source, text=True, check=True)
                command = [binary, 'test', '--no-inherit', '--config', str(config), str(test_file), '--workers', '1']
            if remote_root:
                if options.remote_cache:
                    command = ['env', 'FERRIDRIVER_BROWSERS_PATH=' + options.remote_cache, *command]
                if options.driver_logs:
                    command = ['env', 'RUST_LOG=ferridriver::browser::stderr=debug', *command]
                command_text = shlex.join(command)
                if options.runtime == 'test':
                    command_text = 'cd ' + shlex.quote(remote_root) + ' && ' + command_text
                command = shlex.split(options.ssh_command) + [command_text]
        else:
            bootstrap = root / 'napi.mjs'
            settings = {'module': str(repo / 'crates/ferridriver-node/index.js'),
                        'endpoint': options.endpoint, 'capabilities': capabilities, 'args': args}
            bootstrap.write_text('const settings=' + json.dumps(settings) + ';\n' + '''
const {safari}=await import(settings.module);
const browser=await safari().connect(settings.endpoint,{timeout:120000,capabilities:settings.capabilities});
try {
  const context=browser.contexts()[0],page=(await context.pages())[0];
  const workflow=async(page,context,browser,args)=>{
''' + source + '''
  };
  console.log(JSON.stringify({status:'ok',value:await workflow(page,context,browser,settings.args)}));
} finally {await browser.close();}
''')
            command = ['bun', str(bootstrap)]
        before = simulator_inventory(options, root, 'before') if options.managed else None
        started = time.perf_counter()
        with (root / 'stdout.log').open('w') as stdout, (root / 'stderr.log').open('w') as stderr:
            result = subprocess.run(command, cwd=root, stdout=stdout, stderr=stderr, text=True)
        wall_ms = round((time.perf_counter() - started) * 1000, 2)
        after = simulator_inventory(options, root, 'after') if options.managed else None
        result.stdout = (root / 'stdout.log').read_text()
        result.stderr = (root / 'stderr.log').read_text()
        metadata = {'runtime': options.runtime, 'scenario': options.scenario, 'protocolLogging': options.log_protocol,
                    'managed': options.managed,
                    'workflow': str(source_path),
                    'executionHost': 'ssh' if remote_root else 'local',
                    'binary': options.remote_binary or command[0],
                    'exitCode': result.returncode,
                    'wallMs': wall_ms}
        if options.managed:
            metadata['cleanupVerified'] = before == after
            metadata['cleanupScope'] = ['device UUID/name/state', 'Simulator UI processes', 'xcodebuild processes']
        (root / 'measurement.json').write_text(json.dumps(metadata, indent=2))
        print(result.stdout, end='')
        print(result.stderr, end='')
        print(f'Artifacts: {root}')
        if remote_root:
            screenshot = subprocess.run(shlex.split(options.ssh_command) + [
                'cat ' + shlex.quote(remote_root + '/safari.png'),
            ], capture_output=True)
            if screenshot.returncode == 0:
                (root / 'safari.png').write_bytes(screenshot.stdout)
        if result.returncode:
            raise RuntimeError(f'Workflow exited {result.returncode}')
        if options.managed and before != after:
            raise RuntimeError('Managed launch changed the existing devices, Simulator UI, or driver build inventory')
        if options.runtime == 'test':
            if remote_root:
                report = subprocess.run(shlex.split(options.ssh_command) + [
                    'cat ' + shlex.quote(remote_root + '/workflow-result.json'),
                ], capture_output=True, text=True, check=True)
                (root / 'workflow-result.json').write_text(report.stdout)
            value = json.loads((root / 'workflow-result.json').read_text())
        else:
            value = json.loads(result.stdout)
        if value.get('status') != 'ok' or not (root / 'safari.png').is_file():
            raise RuntimeError('Workflow did not produce its result and screenshot')
    finally:
        tunnel.stdin.close()
        try:
            tunnel.wait(timeout=10)
        except subprocess.TimeoutExpired:
            tunnel.terminate()
            tunnel.wait(timeout=10)
        server.shutdown()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--endpoint', default='http://127.0.0.1:4727')
    parser.add_argument('--device')
    parser.add_argument('--managed', action='store_true')
    parser.add_argument('--model')
    parser.add_argument('--ios-version')
    parser.add_argument('--headless', action='store_true')
    parser.add_argument('--launch-timeout', type=int, default=300000)
    parser.add_argument('--remote-cache')
    parser.add_argument('--ssh-command', required=True)
    parser.add_argument('--remote-port', type=int, default=4779)
    parser.add_argument('--runtime', choices=['script', 'napi', 'test'], default='script')
    parser.add_argument('--scenario', choices=['form', 'frames', 'history', 'tabs', 'reconnect'], default='form')
    parser.add_argument('--workflow')
    parser.add_argument('--log-protocol', action='store_true')
    parser.add_argument('--diagnostics', action='store_true')
    parser.add_argument('--driver-logs', action='store_true')
    parser.add_argument('--remote-binary', help='Run the script with this CLI path on the SSH host')
    options = parser.parse_args()
    if options.remote_binary and options.runtime == 'napi':
        parser.error('--remote-binary requires a native script or test runtime')
    if options.managed and (not options.remote_binary or options.scenario == 'reconnect'):
        parser.error('--managed requires --remote-binary and a workflow that uses its supplied browser')
    if not options.managed and not options.device:
        parser.error('--device is required for a remote endpoint probe')
    run(options)

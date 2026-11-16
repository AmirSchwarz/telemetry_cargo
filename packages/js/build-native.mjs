/** Copy an already-built addon. Run cargo build -p telemetry-node first. */
import { copyFile, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const root = fileURLToPath(new URL('../../', import.meta.url));
const profile = process.argv.includes('--release') ? 'release' : 'debug';
const name = process.platform === 'win32' ? 'telemetry_node.dll' : process.platform === 'darwin' ? 'libtelemetry_node.dylib' : 'libtelemetry_node.so';
const directory = fileURLToPath(new URL('./native/', import.meta.url));
await mkdir(directory, { recursive: true });
await copyFile(join(root, 'target', profile, name), join(directory, 'telemetry.node'));
console.log(join(directory, 'telemetry.node'));

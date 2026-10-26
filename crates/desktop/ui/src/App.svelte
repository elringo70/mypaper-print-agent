<script lang="ts">
	import { api, type AppSnapshot, type JobDto, type CheckDto, type QuoteDto } from './lib/api';

	let snapshot = $state<AppSnapshot | null>(null);
	let selected = $state<string | null>(null);
	let error = $state<string | null>(null);
	let checks = $state<CheckDto[] | null>(null);
	let busy = $state(false);

	const jobs = $derived(snapshot?.jobs ?? []);
	const current = $derived(
		jobs.find((j) => `${j.printer}#${j.jobId}` === selected) ?? jobs[0] ?? null
	);

	// Polling a version stamp is enough: the core bumps it on every change and
	// the snapshot is a few kilobytes, so there is no event stream to desync.
	let seen = -1;
	$effect(() => {
		let alive = true;
		const tick = async () => {
			try {
				const next = await api.snapshot();
				if (alive && next.version !== seen) {
					seen = next.version;
					snapshot = next;
				}
			} catch (e) {
				if (alive) error = String(e);
			}
		};
		tick();
		const id = setInterval(tick, 1000);
		return () => {
			alive = false;
			clearInterval(id);
		};
	});

	async function toggle(job: JobDto, pageNo: number, included: boolean) {
		error = null;
		try {
			await api.togglePage(job.printer, job.jobId, pageNo, included);
		} catch (e) {
			error = String(e);
		}
	}

	async function finish(job: JobDto, action: 'complete' | 'cancel') {
		busy = true;
		error = null;
		try {
			if (action === 'complete') await api.complete(job.printer, job.jobId);
			else await api.cancel(job.printer, job.jobId);
			seen = -1;
		} catch (e) {
			error = String(e);
		} finally {
			busy = false;
		}
	}

	async function setTarget(job: JobDto, kind: 'newDraft' | 'ticket' | 'unassigned', ticketId?: string) {
		error = null;
		try {
			await api.selectTarget(job.printer, job.jobId, kind, ticketId ?? null);
			seen = -1;
		} catch (e) {
			error = String(e);
		}
	}

	async function setPreset(job: JobDto, preset: string) {
		error = null;
		try {
			await api.setPreset(job.printer, job.jobId, preset || null);
			seen = -1;
		} catch (e) {
			error = String(e);
		}
	}

	async function setMode(job: JobDto, mode: 'Bw' | 'Color') {
		error = null;
		try {
			await api.setPrintMode(job.printer, job.jobId, mode);
			seen = -1;
		} catch (e) {
			error = String(e);
		}
	}

	/**
	 * Price per job and rule set, keyed by the snapshot version so any change
	 * (mode, pages) refetches instead of showing a stale total. The number
	 * always comes from the server: a local guess would drift from the ticket.
	 */
	let quotes = $state<Record<string, QuoteDto>>({});
	let quoteError = $state<string | null>(null);

	function quoteKey(job: JobDto) {
		return `${job.printer}#${job.jobId}#${snapshot?.version ?? 0}`;
	}

	async function refreshQuote(job: JobDto) {
		const key = quoteKey(job);
		if (quotes[key]) return;
		try {
			const q = await api.quote(job.printer, job.jobId);
			quotes = { ...quotes, [key]: q };
			quoteError = null;
		} catch (e) {
			quoteError = String(e);
		}
	}

	$effect(() => {
		const job = current;
		if (job && job.pages.length > 0) {
			refreshQuote(job);
			const base = `${job.printer}#${job.jobId}#`;
			for (const k of Object.keys(quotes)) {
				if (k.startsWith(base) && k !== quoteKey(job)) delete quotes[k];
			}
		}
	});

	function quote(job: JobDto) {
		return quotes[quoteKey(job)] ?? null;
	}

	function modeTotal(job: JobDto) {
		const q = quote(job);
		if (!q) return null;
		return job.printMode === 'Bw' ? q.bw.total : q.color.total;
	}

	function pagePrice(job: JobDto, pageNo: number) {
		const q = quote(job);
		if (!q) return null;
		const mode = job.printMode === 'Bw' ? q.bw : q.color;
		return mode.pages.find((p) => p.pageNo === pageNo)?.unitPrice ?? null;
	}

	async function selectAll(job: JobDto, on: boolean) {
		for (const page of job.pages) {
			if (page.included !== on) await toggle(job, page.pageNo, on);
		}
		seen = -1;
	}

	/** Page whose preview fills the right column: the first included one. */
	function previewPage(job: JobDto) {
		return job.pages.find((p) => p.included) ?? job.pages[0] ?? null;
	}

	function money(n: number | null) {
		return n === null ? '—' : `$${n.toFixed(2)}`;
	}

	function included(job: JobDto) {
		return job.pages.filter((p) => p.included).length;
	}

	function classLabel(cls: string | null) {
		switch (cls) {
			case 'color':
				return 'Color';
			case 'gris':
				return 'Gris';
			case 'mono':
				return 'B/N';
			case 'blanco':
				return 'Blanco';
			default:
				return '—';
		}
	}
</script>

<main class="app">
	<header>
		<div class="brand">
			<span class="dot"></span>
			<h1>MyPaper <span class="muted">Print Agent</span></h1>
		</div>
		<div class="status">
			<span class="pill">{snapshot?.mode ?? '…'}</span>
			{#if snapshot?.blocked}
				<span class="pill bad">bloqueado</span>
			{/if}
			<button class="ghost" onclick={async () => (checks = await api.doctor())}>
				Diagnóstico
			</button>
		</div>
	</header>

	{#if snapshot?.blocked}
		<div class="banner">
			<strong>Este equipo no puede cobrar impresiones.</strong>
			{snapshot.blocked}
			<span class="muted">Los trabajos salen sin gestionar.</span>
		</div>
	{/if}

	{#if error}
		<div class="banner error">{error}</div>
	{/if}

	{#if checks}
		<section class="checks">
			{#each checks as c (c.name)}
				<div class="check {c.health}">
					<span class="name">{c.name}</span>
					<span class="detail">{c.detail}</span>
				</div>
			{/each}
			<button class="ghost" onclick={() => (checks = null)}>Cerrar</button>
		</section>
	{/if}

	<div class="body">
		<aside>
			{#if jobs.length === 0}
				<p class="empty">Sin trabajos retenidos.</p>
			{:else}
				{#each jobs as job (`${job.printer}#${job.jobId}`)}
					<button
						class="job"
						class:active={current === job}
						onclick={() => (selected = `${job.printer}#${job.jobId}`)}
					>
						<span class="doc">{job.document}</span>
						<span class="meta">
							{job.printer} · job {job.jobId} · {job.pages.length} pág.
						</span>
						{#if job.analysisError}
							<span class="meta bad">{job.analysisError}</span>
						{/if}
					</button>
				{/each}
			{/if}
		</aside>

		<section class="detail">
			{#if current}
				<header class="head">
					<div>
						<h2>{current.document}</h2>
						<p class="meta">
							{included(current)} de {current.pages.length} páginas ·
							{current.format ?? 'formato desconocido'}
						</p>
					</div>
					<!-- El modo decide el juego de reglas y con él el precio; por eso
					     vive arriba, junto al total, y no escondido entre los ajustes. -->
					<div class="toggle modes" aria-label="Tipo de impresión">
						<button
							class:active={current.printMode === 'Bw'}
							onclick={() => setMode(current, 'Bw')}>B/N</button
						>
						<button
							class:active={current.printMode === 'Color'}
							onclick={() => setMode(current, 'Color')}>Color</button
						>
					</div>
				</header>

				{#if current.warnings.length}
					<ul class="warnings">
						{#each current.warnings as w}<li>{w}</li>{/each}
					</ul>
				{/if}

				<div class="total">
					<span class="label">Total</span>
					<strong>{money(modeTotal(current))}</strong>
					{#if quoteError}
						<span class="small warn">sin precio: {quoteError}</span>
					{/if}
				</div>

				<div class="columns">
					<div class="tariff">
						<h3>Detalles de tarifación</h3>
						<table>
							<thead>
								<tr><th>Pág.</th><th>Tarifa</th><th>Costo</th></tr>
							</thead>
							<tbody>
								{#each current.pages as page (page.pageNo)}
									<tr class:off={!page.included}>
										<td>
											<label class="pick">
												<input
													type="checkbox"
													checked={page.included}
													onchange={(e) => toggle(current, page.pageNo, e.currentTarget.checked)}
												/>
												{page.pageNo}
											</label>
										</td>
										<td>
											<span class="chip">{classLabel(page.class)}</span>
											<span class="small muted">
												{page.areaPct !== null ? `${page.areaPct.toFixed(1)}%` : '—'}
											</span>
										</td>
										<td class="cost">{money(pagePrice(current, page.pageNo))}</td>
									</tr>
								{/each}
							</tbody>
						</table>
						<label class="pick all">
							<input
								type="checkbox"
								checked={current.pages.length > 0 && included(current) === current.pages.length}
								onchange={(e) => selectAll(current, e.currentTarget.checked)}
							/>
							Seleccionar todas las páginas
						</label>
					</div>

					<div class="preview">
						{#if previewPage(current)}
							<img src={previewPage(current)?.thumbnail} alt={`Página ${previewPage(current)?.pageNo}`} />
						{:else}
							<div class="placeholder">sin vista previa</div>
						{/if}
						{#if previewPage(current)}
							<p class="ink small">
								Negro: {(previewPage(current)?.k ?? 0).toFixed(2)}% ·
								Color: {Math.max(
									previewPage(current)?.c ?? 0,
									previewPage(current)?.m ?? 0,
									previewPage(current)?.y ?? 0
								).toFixed(2)}% ·
								Blanco: {(100 - (previewPage(current)?.areaPct ?? 0)).toFixed(2)}%
							</p>
						{/if}
					</div>
				</div>

				<div class="controls">
					<div class="field">
						<span class="label">Destino</span>
						<select
							value={current.target === 'ticket' ? `ticket:${current.targetTicketId}` : current.target}
							onchange={(e) => {
								const v = e.currentTarget.value;
								if (v.startsWith('ticket:')) setTarget(current, 'ticket', v.slice(7));
								else setTarget(current, v as 'newDraft' | 'unassigned');
							}}
						>
							<option value="unassigned">Sin asignar</option>
							<option value="newDraft">Nuevo borrador</option>
							{#each snapshot?.tickets ?? [] as t (t.id)}
								<option value={`ticket:${t.id}`}>Ticket #{t.number} · {t.cashier} · ${t.total}</option>
							{/each}
						</select>
					</div>

					<div class="field">
						<span class="label">Preset</span>
						<select value={current.preset ?? ''} onchange={(e) => setPreset(current, e.currentTarget.value)}>
							<option value="">Del controlador</option>
							{#each snapshot?.presets ?? [] as name (name)}
								<option value={name}>{name}</option>
							{/each}
						</select>
					</div>
				</div>

				<footer>
					<button class="ghost" disabled={busy} onclick={() => finish(current, 'cancel')}>
						Cancelar
					</button>
					<button
						class="primary"
						disabled={busy || included(current) === 0}
						onclick={() => finish(current, 'complete')}
					>
						Imprimir
					</button>
				</footer>
			{:else}
				<p class="empty">Selecciona un trabajo para revisar sus páginas.</p>
			{/if}
		</section>
	</div>
</main>

<style>
	.head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}
	.modes button {
		min-width: 64px;
	}
	.total {
		display: flex;
		align-items: baseline;
		gap: 10px;
		padding: 12px 14px;
		border-radius: 10px;
		background: #131a24;
		border: 1px solid #1f2a38;
	}
	.total strong {
		font-size: 30px;
		color: #6ee7a8;
	}
	.columns {
		display: grid;
		grid-template-columns: minmax(260px, 1fr) minmax(220px, 0.8fr);
		gap: 16px;
		align-items: start;
	}
	.tariff table {
		width: 100%;
		border-collapse: collapse;
	}
	.tariff th,
	.tariff td {
		padding: 6px 8px;
		text-align: left;
		border-bottom: 1px solid #1b2431;
		font-size: 13px;
	}
	.tariff tr.off td {
		opacity: 0.45;
	}
	.cost {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.pick {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.pick.all {
		margin-top: 10px;
		font-size: 12px;
		color: #9fb0c6;
	}
	.preview {
		background: #0b0f15;
		border: 1px solid #1b2431;
		border-radius: 10px;
		padding: 10px;
		text-align: center;
	}
	.preview img {
		max-width: 100%;
		max-height: 260px;
		border-radius: 6px;
	}
	.ink {
		margin: 8px 0 0;
	}
	:global(html, body) {
		margin: 0;
		height: 100%;
		background: #0f1218;
		color: #e6e9ef;
		font:
			14px/1.45 system-ui,
			-apple-system,
			'Segoe UI',
			sans-serif;
	}
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 14px 18px;
		border-bottom: 1px solid #1e2430;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.dot {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: #38bdf8;
		box-shadow: 0 0 12px #38bdf8;
	}
	h1 {
		font-size: 15px;
		font-weight: 600;
		margin: 0;
		letter-spacing: 0.2px;
	}
	h2 {
		font-size: 15px;
		margin: 0 0 2px;
	}
	.muted {
		color: #7d8798;
		font-weight: 400;
	}
	.status {
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.pill {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.6px;
		padding: 3px 8px;
		border-radius: 999px;
		background: #1b2330;
		color: #9fb0c7;
	}
	.pill.bad {
		background: #3b1d1d;
		color: #f8a5a5;
	}
	.banner {
		margin: 10px 18px 0;
		padding: 10px 14px;
		border-radius: 10px;
		background: #241c10;
		border: 1px solid #4a3413;
		color: #f0c98a;
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.banner.error {
		background: #2a1518;
		border-color: #52232a;
		color: #f3a8b1;
	}
	.checks {
		margin: 10px 18px 0;
		padding: 10px;
		border: 1px solid #1e2430;
		border-radius: 10px;
		display: grid;
		gap: 6px;
	}
	.check {
		display: flex;
		gap: 10px;
		font-size: 12px;
	}
	.check .name {
		width: 90px;
		color: #9fb0c7;
	}
	.check.fail .detail {
		color: #f3a8b1;
	}
	.check.warn .detail {
		color: #f0c98a;
	}
	.body {
		display: grid;
		grid-template-columns: 300px 1fr;
		flex: 1;
		min-height: 0;
	}
	aside {
		border-right: 1px solid #1e2430;
		overflow: auto;
		padding: 10px;
		display: grid;
		gap: 8px;
		align-content: start;
	}
	.job {
		text-align: left;
		background: #141a23;
		border: 1px solid #1e2430;
		border-radius: 10px;
		padding: 10px;
		color: inherit;
		display: grid;
		gap: 3px;
		cursor: pointer;
	}
	.job.active {
		border-color: #38bdf8;
		background: #16202b;
	}
	.doc {
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.meta {
		font-size: 12px;
		color: #7d8798;
	}
	.meta.bad {
		color: #f3a8b1;
	}
	.detail {
		display: flex;
		flex-direction: column;
		min-height: 0;
		padding: 14px 18px;
	}
	.warnings {
		margin: 6px 0 0;
		padding-left: 18px;
		color: #f0c98a;
		font-size: 12px;
	}
	.controls {
		display: flex;
		gap: 18px;
		align-items: flex-end;
		flex-wrap: wrap;
		margin-top: 12px;
		padding: 10px 12px;
		border: 1px solid #1e2430;
		border-radius: 10px;
		background: #12171f;
	}
	.field {
		display: grid;
		gap: 4px;
	}
	.label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.6px;
		color: #7d8798;
	}
	select {
		background: #0f141b;
		color: #e6e9ef;
		border: 1px solid #2a3442;
		border-radius: 8px;
		padding: 6px 8px;
		font: inherit;
		min-width: 170px;
	}
	.toggle {
		display: inline-flex;
		border: 1px solid #2a3442;
		border-radius: 8px;
		overflow: hidden;
	}
	.toggle button {
		background: transparent;
		color: #c8d3e2;
		border: 0;
		padding: 6px 12px;
	}
	.toggle button.active {
		background: #38bdf8;
		color: #06202b;
		font-weight: 600;
	}
	.pages {
		margin-top: 12px;
		overflow: auto;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
		gap: 10px;
		align-content: start;
		flex: 1;
	}
	.page {
		border: 1px solid #1e2430;
		border-radius: 10px;
		padding: 8px;
		display: grid;
		gap: 6px;
		cursor: pointer;
		background: #12171f;
	}
	.page.off {
		opacity: 0.45;
	}
	.thumb {
		aspect-ratio: 3 / 4;
		background: #fff;
		border-radius: 6px;
		overflow: hidden;
		display: grid;
		place-items: center;
	}
	.placeholder {
		color: #99a;
		font-size: 11px;
	}
	.page-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-wrap: wrap;
		font-size: 11px;
	}
	.num {
		font-weight: 600;
	}
	.chip {
		background: #1b2330;
		border-radius: 999px;
		padding: 1px 7px;
		color: #9fb0c7;
	}
	.small {
		font-size: 11px;
	}
	footer {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		padding-top: 12px;
	}
	button {
		font: inherit;
		cursor: pointer;
	}
	.primary {
		background: #38bdf8;
		color: #06202b;
		border: 0;
		border-radius: 8px;
		padding: 8px 14px;
		font-weight: 600;
	}
	.ghost {
		background: transparent;
		color: #c8d3e2;
		border: 1px solid #2a3442;
		border-radius: 8px;
		padding: 7px 12px;
	}
	button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.empty {
		color: #7d8798;
		padding: 18px;
	}
</style>
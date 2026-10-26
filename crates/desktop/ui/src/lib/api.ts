import { invoke } from '@tauri-apps/api/core';

export interface PageDto {
	pageNo: number;
	c: number;
	m: number;
	y: number;
	k: number;
	areaPct: number | null;
	class: string | null;
	tierId: string | null;
	included: boolean;
	thumbnail: string;
}

export interface JobDto {
	printer: string;
	jobId: number;
	document: string;
	state: string;
	pages: PageDto[];
	analysisError: string | null;
	warnings: string[];
	format: string | null;
	target: 'unassigned' | 'newDraft' | 'ticket';
	targetTicketId: string | null;
	preset: string | null;
	printMode: 'Bw' | 'Color';
	price: number | null;
}

export interface TicketDto {
	id: string;
	number: number;
	cashier: string;
	total: number;
}

export interface AppSnapshot {
	version: number;
	mode: string;
	blocked: string | null;
	jobs: JobDto[];
	tickets: TicketDto[];
	presets: string[];
}

export interface CheckDto {
	name: string;
	health: 'ok' | 'warn' | 'fail';
	detail: string;
}

/**
 * The whole bridge to Rust. Four calls, no events: polling a version stamp is
 * simpler to reason about than a live subscription, and the snapshot is small.
 */
export const api = {
	snapshot: () => invoke<AppSnapshot>('snapshot'),
	togglePage: (printer: string, jobId: number, pageNo: number, included: boolean) =>
		invoke<void>('toggle_page', { printer, jobId, pageNo, included }),
	complete: (printer: string, jobId: number) =>
		invoke<void>('complete_job', { printer, jobId }),
	cancel: (printer: string, jobId: number) => invoke<void>('cancel_job', { printer, jobId }),
	selectTarget: (
		printer: string,
		jobId: number,
		kind: 'unassigned' | 'newDraft' | 'ticket',
		ticketId?: string | null
	) => invoke<void>('select_target', { printer, jobId, kind, ticketId: ticketId ?? null }),
	setPreset: (printer: string, jobId: number, preset: string | null) =>
		invoke<void>('set_preset', { printer, jobId, preset }),
	setPrintMode: (printer: string, jobId: number, mode: 'Bw' | 'Color') =>
		invoke<void>('set_print_mode', { printer, jobId, mode }),
	doctor: () => invoke<CheckDto[]>('doctor')
};
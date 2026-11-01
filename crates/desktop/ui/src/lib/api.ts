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
	/** No ink at all: not charged and not printed. */
	blank: boolean;
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
	/** Customer of the sale, when it has one. */
	customer: string | null;
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

export interface PageQuoteDto {
	pageNo: number;
	unitPrice: number;
}

export interface ModeQuoteDto {
	total: number;
	pages: PageQuoteDto[];
}

/** Both rule sets, so flipping B/N ↔ Color never waits on a round trip. */
export interface QuoteDto {
	bw: ModeQuoteDto;
	color: ModeQuoteDto;
}

export interface CheckDto {
	name: string;
	health: 'ok' | 'warn' | 'fail';
	detail: string;
}

/**
 * The whole bridge to Rust. Calls only, no events: the window polls a version
 * stamp (one number) and reads the snapshot only when it moved, so no event
 * stream can desync. Page previews are fetched one at a time, on demand.
 */
export const api = {
	version: () => invoke<number>('version'),
	snapshot: () => invoke<AppSnapshot>('snapshot'),
	pageThumbnail: (printer: string, jobId: number, pageNo: number) =>
		invoke<string>('page_thumbnail', { printer, jobId, pageNo }),
	togglePage: (printer: string, jobId: number, pageNo: number, included: boolean) =>
		invoke<void>('toggle_page', { printer, jobId, pageNo, included }),
	complete: (printer: string, jobId: number) =>
		invoke<void>('complete_job', { printer, jobId }),
	cancel: (printer: string, jobId: number) => invoke<void>('cancel_job', { printer, jobId }),
	quote: (printer: string, jobId: number) => invoke<QuoteDto>('quote', { printer, jobId }),
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
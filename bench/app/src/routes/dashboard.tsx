import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';

interface Sale {
  id: string;
  region: 'NA' | 'EU' | 'APAC';
  product: string;
  amount: number;
  date: string;
  status: 'pending' | 'shipped' | 'delivered' | 'returned';
}

async function fetchSales(): Promise<Sale[]> {
  const r = await fetch('/api/sales');
  if (!r.ok) throw new Error(`sales: ${r.status}`);
  return r.json();
}

export function Dashboard() {
  const { data, isLoading, error } = useQuery({
    queryKey: ['sales'],
    queryFn: fetchSales,
  });
  const [region, setRegion] = useState<'all' | Sale['region']>('all');
  const [status, setStatus] = useState<'all' | Sale['status']>('all');
  const [sortBy, setSortBy] = useState<'amount' | 'date'>('amount');

  const filtered = useMemo(() => {
    if (!data) return [];
    let out = data.slice();
    if (region !== 'all') out = out.filter((s) => s.region === region);
    if (status !== 'all') out = out.filter((s) => s.status === status);
    if (sortBy === 'amount') out.sort((a, b) => b.amount - a.amount);
    if (sortBy === 'date') out.sort((a, b) => (a.date < b.date ? 1 : -1));
    return out;
  }, [data, region, status, sortBy]);

  const totalAmount = useMemo(
    () => filtered.reduce((sum, s) => sum + s.amount, 0),
    [filtered],
  );

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold" data-testid="dashboard-title">
        Sales Dashboard
      </h1>
      <div className="card flex flex-wrap gap-3 items-end">
        <label className="flex flex-col gap-1">
          <span className="text-sm">region</span>
          <select
            className="input"
            data-testid="region-filter"
            value={region}
            onChange={(e) => setRegion(e.target.value as typeof region)}
          >
            <option value="all">all</option>
            <option value="NA">NA</option>
            <option value="EU">EU</option>
            <option value="APAC">APAC</option>
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-sm">status</span>
          <select
            className="input"
            data-testid="status-filter"
            value={status}
            onChange={(e) => setStatus(e.target.value as typeof status)}
          >
            <option value="all">all</option>
            <option value="pending">pending</option>
            <option value="shipped">shipped</option>
            <option value="delivered">delivered</option>
            <option value="returned">returned</option>
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-sm">sort</span>
          <select
            className="input"
            data-testid="sort-by"
            value={sortBy}
            onChange={(e) => setSortBy(e.target.value as typeof sortBy)}
          >
            <option value="amount">by amount</option>
            <option value="date">by date</option>
          </select>
        </label>
        <div className="ml-auto flex flex-col items-end">
          <span className="text-xs text-[var(--fg)]/60">total</span>
          <span className="text-xl font-semibold" data-testid="total-amount">
            ${totalAmount.toLocaleString()}
          </span>
        </div>
      </div>

      {isLoading && <div data-testid="dashboard-loading">loading…</div>}
      {error && (
        <div className="error" data-testid="dashboard-error">
          {(error as Error).message}
        </div>
      )}
      <div className="card overflow-x-auto">
        <table className="w-full text-left text-sm" data-testid="sales-table">
          <thead className="border-b border-[var(--border)]">
            <tr>
              <th className="p-2">id</th>
              <th className="p-2">region</th>
              <th className="p-2">product</th>
              <th className="p-2 text-right">amount</th>
              <th className="p-2">date</th>
              <th className="p-2">status</th>
            </tr>
          </thead>
          <tbody data-testid="sales-rows">
            {filtered.map((s) => (
              <tr key={s.id} data-testid={`row-${s.id}`} className="border-b border-[var(--border)]">
                <td className="p-2 font-mono text-xs">{s.id}</td>
                <td className="p-2">{s.region}</td>
                <td className="p-2">{s.product}</td>
                <td className="p-2 text-right" data-testid={`amount-${s.id}`}>
                  ${s.amount.toLocaleString()}
                </td>
                <td className="p-2">{s.date}</td>
                <td className="p-2">
                  <span
                    data-testid={`status-${s.id}`}
                    className="bg-[var(--border)] px-2 py-0.5 rounded text-xs"
                  >
                    {s.status}
                  </span>
                </td>
              </tr>
            ))}
            {filtered.length === 0 && (
              <tr>
                <td colSpan={6} className="text-center p-6 text-[var(--fg)]/60" data-testid="no-rows">
                  no rows match filters
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      <div className="text-sm text-[var(--fg)]/60" data-testid="row-count">
        {filtered.length} rows
      </div>
    </div>
  );
}

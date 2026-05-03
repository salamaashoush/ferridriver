import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Link, useParams } from 'react-router-dom';

interface Post {
  slug: string;
  title: string;
  excerpt: string;
  body: string;
  tags: string[];
  date: string;
}

async function fetchPosts(): Promise<Post[]> {
  const r = await fetch('/api/posts');
  if (!r.ok) throw new Error(`posts: ${r.status}`);
  return r.json();
}

async function fetchPost(slug: string): Promise<Post> {
  const r = await fetch(`/api/posts/${slug}`);
  if (!r.ok) throw new Error(`post ${slug}: ${r.status}`);
  return r.json();
}

export function BlogList() {
  const { data, isLoading, error } = useQuery({
    queryKey: ['posts'],
    queryFn: fetchPosts,
  });
  const [search, setSearch] = useState('');
  const [page, setPage] = useState(0);
  const PAGE_SIZE = 10;

  const filtered = useMemo(() => {
    if (!data) return [];
    const q = search.toLowerCase();
    return data.filter(
      (p) =>
        !q ||
        p.title.toLowerCase().includes(q) ||
        p.tags.some((t) => t.toLowerCase().includes(q)),
    );
  }, [data, search]);

  const visible = filtered.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);
  const lastPage = Math.max(0, Math.ceil(filtered.length / PAGE_SIZE) - 1);

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold" data-testid="blog-title">
        Blog
      </h1>
      <div className="flex gap-2">
        <input
          className="input"
          placeholder="search…"
          value={search}
          data-testid="blog-search"
          onChange={(e) => {
            setSearch(e.target.value);
            setPage(0);
          }}
        />
        <span className="self-center text-[var(--fg)]/70" data-testid="blog-count">
          {filtered.length} matches
        </span>
      </div>
      {isLoading && <div data-testid="blog-loading">loading…</div>}
      {error && (
        <div className="error" data-testid="blog-error">
          {(error as Error).message}
        </div>
      )}
      <ul className="space-y-2" data-testid="blog-list">
        {visible.map((p) => (
          <li key={p.slug} className="card" data-testid={`post-${p.slug}`}>
            <Link
              to={`/blog/${p.slug}`}
              className="font-semibold text-lg"
              data-testid={`post-link-${p.slug}`}
            >
              {p.title}
            </Link>
            <p className="text-[var(--fg)]/70 mt-1">{p.excerpt}</p>
            <div className="mt-2 flex gap-1 text-xs">
              {p.tags.map((t) => (
                <span
                  key={t}
                  data-testid={`tag-${t}`}
                  className="bg-[var(--border)] px-2 py-0.5 rounded"
                >
                  {t}
                </span>
              ))}
            </div>
          </li>
        ))}
      </ul>
      <div className="flex gap-2 items-center justify-between">
        <button
          className="btn-ghost btn"
          data-testid="prev-page"
          disabled={page === 0}
          onClick={() => setPage((p) => Math.max(0, p - 1))}
        >
          prev
        </button>
        <span data-testid="page-info">
          page {page + 1} / {lastPage + 1}
        </span>
        <button
          className="btn-ghost btn"
          data-testid="next-page"
          disabled={page >= lastPage}
          onClick={() => setPage((p) => Math.min(lastPage, p + 1))}
        >
          next
        </button>
      </div>
    </div>
  );
}

export function BlogPost() {
  const { slug = '' } = useParams();
  const { data, isLoading, error } = useQuery({
    queryKey: ['post', slug],
    queryFn: () => fetchPost(slug),
    enabled: !!slug,
  });

  if (isLoading) return <div data-testid="post-loading">loading…</div>;
  if (error)
    return (
      <div className="error" data-testid="post-error">
        {(error as Error).message}
      </div>
    );
  if (!data) return null;

  return (
    <article className="card prose-invert max-w-none">
      <h1 className="text-3xl font-bold" data-testid="post-title">
        {data.title}
      </h1>
      <div className="text-sm text-[var(--fg)]/60 mb-4" data-testid="post-date">
        {data.date}
      </div>
      <p className="whitespace-pre-wrap" data-testid="post-body">
        {data.body}
      </p>
      <div className="mt-4 flex gap-1">
        {data.tags.map((t) => (
          <span
            key={t}
            data-testid={`post-tag-${t}`}
            className="bg-[var(--border)] px-2 py-0.5 rounded text-xs"
          >
            {t}
          </span>
        ))}
      </div>
      <Link to="/blog" className="btn btn-ghost mt-6 inline-block" data-testid="back-link">
        ← back
      </Link>
    </article>
  );
}

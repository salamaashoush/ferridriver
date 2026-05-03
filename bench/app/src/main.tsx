import React from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter, NavLink, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import './index.css';
import { TodosPage } from './routes/todos';
import { BlogList, BlogPost } from './routes/blog';
import { Dashboard } from './routes/dashboard';
import { FormsPage } from './routes/forms';
import { Wizard } from './routes/wizard';

const qc = new QueryClient({
  defaultOptions: { queries: { staleTime: 30_000, retry: 0 } },
});

function Layout({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col min-h-full">
      <header className="border-b border-[var(--border)] sticky top-0 bg-[var(--bg)] z-10">
        <nav className="container flex items-center gap-2 py-3">
          <span className="font-bold mr-4" data-testid="brand">
            kitchen-sink
          </span>
          <NavLink to="/todos" className="nav-link" data-testid="nav-todos">
            todos
          </NavLink>
          <NavLink to="/blog" className="nav-link" data-testid="nav-blog">
            blog
          </NavLink>
          <NavLink to="/dashboard" className="nav-link" data-testid="nav-dashboard">
            dashboard
          </NavLink>
          <NavLink to="/forms" className="nav-link" data-testid="nav-forms">
            forms
          </NavLink>
          <NavLink to="/wizard" className="nav-link" data-testid="nav-wizard">
            wizard
          </NavLink>
        </nav>
      </header>
      <main className="container flex-1 py-6">{children}</main>
    </div>
  );
}

function Home() {
  return (
    <div className="card">
      <h1 className="text-2xl font-semibold" data-testid="home-title">
        kitchen sink bench
      </h1>
      <p className="mt-2 text-[var(--fg)]/70">
        A multi-route React + shadcn-style app. Navigate via the header to exercise todos, blog,
        dashboard async data, forms, and a multi-step wizard.
      </p>
    </div>
  );
}

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <Layout>
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/todos" element={<TodosPage />} />
            <Route path="/blog" element={<BlogList />} />
            <Route path="/blog/:slug" element={<BlogPost />} />
            <Route path="/dashboard" element={<Dashboard />} />
            <Route path="/forms" element={<FormsPage />} />
            <Route path="/wizard" element={<Wizard />} />
          </Routes>
        </Layout>
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>,
);

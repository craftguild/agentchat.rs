import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router';
import { ViewerProvider } from './graphql/provider.tsx';
import { SessionProvider } from './state/session.tsx';
import { App } from './app.tsx';
import './fonts.css';
import './global.css';
import './message.css';
import './app.css';

createRoot(document.getElementById('root')!).render(<StrictMode><BrowserRouter><ViewerProvider><SessionProvider><App/></SessionProvider></ViewerProvider></BrowserRouter></StrictMode>);

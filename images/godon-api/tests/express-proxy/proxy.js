const express = require('express');
const { createProxyMiddleware } = require('http-proxy-middleware');
const axios = require('axios');
const http = require('http');

const app = express();
const PORT = 8000;
const PRISM_URL = 'http://172.17.0.4:4010'; // Prism container IP

// NOTE: Minimal body parsing only for specific endpoints that need it
// We use conditional parsing to avoid creating empty {} objects for GET requests
app.use((req, res, next) => {
  // Only parse JSON for POST/PUT/PATCH requests
  if (['POST', 'PUT', 'PATCH'].includes(req.method)) {
    express.json()(req, res, next);
  } else {
    next();
  }
});

// Mock responses for specific flow paths (decoded URLs)
const mockResponses = {
  '/w/godon/jobs/run_wait_result/p/f/controller/breeders_get': {
    breeders: [
      {
        id: "550e8400-e29b-41d4-a716-446655440010",
        name: "optimizer-test",
        status: "active",
        createdAt: "2024-01-15T10:30:00Z",
        config: {
          step_size: 200,
          max_iterations: 10
        }
      },
      {
        id: "550e8400-e29b-41d4-a716-446655440011",
        name: "some-test",
        status: "inactive",
        createdAt: "2024-01-10T15:45:00Z",
        config: {
          step_size: 0.01,
          max_iterations: 1000
        }
      }
    ]
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_create': {
    id: "550e8400-e29b-41d4-a716-446655440010",
    name: "test_breeder",
    status: "active",
    createdAt: "2024-01-15T10:30:00Z"
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_delete': {
    success: true
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_update': {
    id: "550e8400-e29b-41d4-a716-446655440010",
    name: "updated-genetic-optimizer",
    status: "active",
    createdAt: "2024-01-15T10:30:00Z",
    config: {
      setting1: "new_value1",
      setting2: 200
    }
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_get': {
    id: "550e8400-e29b-41d4-a716-446655440010",
    name: "genetic-optimizer-test",
    status: "active",
    createdAt: "2024-01-15T10:30:00Z",
    config: {
      step_size: 3,
      max_iterations: 100
    }
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/credentials_get': [
    {
      id: "550e8400-e29b-41d4-a716-446655440011",
      name: "production_ssh_key",
      credentialType: "ssh_private_key",
      description: "SSH key for production servers",
        windmillVariable: "f/vars/prod_ssh_key",
      createdAt: "2024-01-15T10:30:00Z",
      lastUsedAt: "2024-01-16T14:20:00Z"
    },
    {
      id: "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      name: "staging_ssh_key",
      credentialType: "ssh_private_key",
      description: "SSH key for staging environment",
      windmillVariable: "f/vars/staging_ssh_key",
      createdAt: "2024-01-10T15:45:00Z",
      lastUsedAt: null
    }
  ],
  '/w/godon/jobs/run_wait_result/p/f/controller/credential_create': {
    id: "550e8400-e29b-41d4-a716-446655440002",
    name: "test_ssh_key",
    credentialType: "ssh_private_key",
    description: "Test SSH key",
    windmillVariable: "f/vars/test_ssh_key",
    createdAt: "2024-01-17T12:00:00Z"
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/credential_get': {
    id: "550e8400-e29b-41d4-a716-446655440011",
    name: "production_ssh_key",
    credentialType: "ssh_private_key",
    description: "SSH key for production servers",
    windmillVariable: "f/vars/prod_ssh_key",
    createdAt: "2024-01-15T10:30:00Z",
    lastUsedAt: "2024-01-16T14:20:00Z",
    content: "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA2Z9Q...\n-----END RSA PRIVATE KEY-----"
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/credential_delete': {
    result: "SUCCESS",
    message: "Credential 'production_ssh_key' (ID: 550e8400-e29b-41d4-a716-446655440011) successfully deleted"
  }
};

// Custom middleware that proxies to Prism and transforms responses
const windmillProxy = async (req, res, next) => {
  try {
    console.log(`${req.method} ${req.originalUrl}`);
    
    // Debug: Log request headers for Windmill variable endpoints
    if (req.originalUrl.includes('/variables/')) {
      console.log(`🔍 DEBUG Request headers:`, JSON.stringify(req.headers, null, 2));
      console.log(`🔍 DEBUG Content-Type:`, req.headers['content-type']);
    }

    // Special handling for auth endpoint - returns plaintext token
    if ((req.originalUrl === '/api/auth/login' || req.originalUrl === '/auth/login') && req.method === 'POST') {
      console.log(`🎯 Auth mock found - returning plaintext token`);
      return res.type('text/plain').send('mock_bearer_token_for_testing');
    }
    
    // Decode the URL for matching
    let decodedUrl = decodeURIComponent(req.originalUrl);

    // Handle /api/w/ prefix - remove it for mock matching
    if (decodedUrl.startsWith('/api/w/')) {
      decodedUrl = decodedUrl.replace('/api/w/', '/w/');
    }
    
    console.log(`🔍 Debug: originalUrl = "${req.originalUrl}"`);
    console.log(`🔍 Debug: decodedUrl = "${decodedUrl}"`);
    console.log(`🔍 Debug: available mocks = ${Object.keys(mockResponses).join(', ')}`);
    
    // Check if we have a mock response for this decoded path
    if (mockResponses[decodedUrl]) {
      console.log(`🎯 Mock found for: ${req.originalUrl} - returning transformed response`);
      return res.json(mockResponses[decodedUrl]);
    }
    
    // For non-mocked paths, proxy to Prism normally
    console.log(`❌ No mock found - forwarding to Prism: ${decodedUrl}`);
    
    // Prepare request config for axios - don't send body for GET/HEAD requests
    let axiosConfig = {
      method: req.method,
      url: `${PRISM_URL}${decodedUrl}`,
      headers: req.headers,
      timeout: 5000
    };
    
    // Only include body data for methods that commonly have bodies
    if (req.method !== 'GET' && req.method !== 'HEAD' && req.body) {
      axiosConfig.data = req.body;
    }
    
    const response = await axios(axiosConfig);
    
    // Forward Prism's response
    res.status(response.status).json(response.data);
    
  } catch (error) {
    console.error(`❌ Proxy error for ${req.originalUrl}:`, error.message);
    
    if (error.response) {
      // Prism responded with an error
      console.log(`❌ Prism error response: ${error.response.status} - ${JSON.stringify(error.response.data)}`);
      res.status(error.response.status).json(error.response.data);
    } else if (error.code === 'ECONNABORTED') {
      res.status(504).json({ error: 'Gateway Timeout' });
    } else {
      res.status(502).json({ error: 'Bad Gateway' });
    }
  }
};

// Health check endpoint
app.get('/health', (req, res) => {
  res.json({ 
    status: 'healthy', 
    service: 'express-proxy',
    timestamp: new Date().toISOString(),
    mocked_endpoints: Object.keys(mockResponses).length
  });
});

// Apply proxy middleware to all paths
app.use('/', windmillProxy);

// Start server
app.listen(PORT, '0.0.0.0', () => {
  console.log(`🚀 Windmill Express Proxy running on port ${PORT}`);
  console.log(`📡 Forwarding to Prism at: ${PRISM_URL}`);
  console.log(`🎯 Mocked endpoints: ${Object.keys(mockResponses).length}`);
  console.log(`❤️  Health check: http://localhost:${PORT}/health`);
});

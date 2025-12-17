const express = require('express');
const { createProxyMiddleware } = require('http-proxy-middleware');
const axios = require('axios');

const app = express();
const PORT = 8000;
const PRISM_URL = 'http://172.17.0.8:4010'; // Prism container IP

// Middleware to parse JSON bodies
app.use(express.json());

// Mock responses for specific flow paths (decoded URLs)
const mockResponses = {
  '/w/godon/jobs/run_wait_result/p/f/controller/breeders_get': {
    breeders: [
      {
        id: "550e8400-e29b-41d4-a716-446655440000",
        name: "optimizer-test",
        status: "active",
        createdAt: "2024-01-15T10:30:00Z",
        config: {
          step_size: 200,
          max_iterations: 10
        }
      },
      {
        id: "550e8400-e29b-41d4-a716-446655440001",
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
    id: "550e8400-e29b-41d4-a716-446655440000",
    name: "test_breeder",
    status: "active",
    createdAt: "2024-01-15T10:30:00Z"
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_delete': {
    success: true
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_update': {
    breeder_id: "550e8400-e29b-41d4-a716-446655440000",
    updated: true
  },
  '/w/godon/jobs/run_wait_result/p/f/controller/breeder_get': {
    id: "550e8400-e29b-41d4-a716-446655440000",
    name: "genetic-optimizer-test",
    status: "active",
    config: {
      step_size: 3,
      max_iterations: 100
    },
    created_at: "2024-01-15T10:30:00Z",
    updated_at: "2024-01-20T14:22:00Z"
  }
};

// Custom middleware that proxies to Prism and transforms responses
const windmillProxy = async (req, res, next) => {
  try {
    console.log(`${req.method} ${req.originalUrl}`);

    // Decode the URL for matching
    const decodedUrl = decodeURIComponent(req.originalUrl);
    console.log(`🔍 Debug: originalUrl = "${req.originalUrl}"`);
    console.log(`🔍 Debug: decodedUrl = "${decodedUrl}"`);
    console.log(`🔍 Debug: available mocks = ${Object.keys(mockResponses).join(', ')}`);

    // Check if we have a mock response for this decoded path
    if (mockResponses[decodedUrl]) {
      console.log(`🎯 Mock found for: ${req.originalUrl} - returning transformed response`);
      return res.json(mockResponses[decodedUrl]);
    }
    
    // For non-mocked paths, proxy to Prism normally
    console.log(`❌ No mock found - forwarding to Prism: ${req.originalUrl}`);
    
    const response = await axios({
      method: req.method,
      url: `${PRISM_URL}${req.originalUrl}`,
      headers: req.headers,
      data: req.body,
      timeout: 5000
    });
    
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
